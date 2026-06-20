// Bakes a static SEO snapshot of the dc-runtime pages (home + guide) so crawlers
// see real content instead of {{ token }} placeholders. The dc-runtime reads its
// template from <x-dc> then replaces it with <div id="dc-root">; we hide <x-dc>,
// add a sibling #seo-prerender (the rendered default-language HTML, scripts/iframes
// stripped) and a CSS rule that hides it once #dc-root is populated.
//
// Self-contained: starts its own static server (extensionless routing) and resolves
// Playwright from local node_modules (CI) or the main repo (local dev). Idempotent.
// Run: node scripts/build-prerender.mjs   (CI runs it via .github/workflows/prerender.yml)
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync, existsSync, statSync } from 'node:fs';
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
const PAGES = [
  { file: 'index.html', path: '/' },
  { file: 'guide.html', path: '/guide' },
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

const server = await startServer();
const browser = await chromium.launch({ args: ['--no-sandbox', '--disable-dev-shm-usage'] });
let changed = 0;
for (const p of PAGES) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  await page.goto(`http://127.0.0.1:${PORT}${p.path}`, { waitUntil: 'load' });
  await page.evaluate(() => { try { localStorage.clear(); } catch (e) {} });   // default lang (en) + default theme
  await page.reload({ waitUntil: 'load' });
  await page.waitForFunction(
    () => { const r = document.getElementById('dc-root'); return r && r.children.length && !/\{\{/.test(r.textContent); },
    { timeout: 30000 });
  await page.waitForTimeout(600);
  const html = await page.evaluate(() => {
    const clone = document.getElementById('dc-root').cloneNode(true);
    clone.querySelectorAll('script, iframe').forEach((e) => e.remove());
    return clone.innerHTML;
  });
  await page.close();

  let s = readFileSync(join(SITE, p.file), 'utf8');
  const before = s;
  s = s.replace(/<style id="seo-prerender-css">[\s\S]*?<\/style>/g, '');
  s = s.replace(new RegExp(START + '[\\s\\S]*?' + END, 'g'), '');
  s = s.replace('</head>', CSS + '</head>');
  const block = START + '<div id="seo-prerender">' + html + '</div>' + END;
  // Inject before the <x-dc> open tag (function replacer: html may contain $&).
  s = s.replace(/<x-dc(\s|>)/, (m) => block + m);
  if (s !== before) { writeFileSync(join(SITE, p.file), s); changed++; }
  console.log(`  ${p.file}: prerender ${Math.round(html.length / 1024)} KB (${s !== before ? 'updated' : 'unchanged'})`);
}
await browser.close();
server.close();
console.log(`OK prerender (${changed} file(s) updated)`);
