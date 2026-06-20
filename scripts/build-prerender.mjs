// Bakes a static SEO snapshot of the dc-runtime pages (home + guide) so crawlers
// see real content instead of {{ token }} placeholders. The dc-runtime reads its
// template from <x-dc> then replaces it with <div id="dc-root">; we hide <x-dc>,
// add a sibling #seo-prerender (the rendered default-language HTML, scripts/iframes
// stripped), and a CSS rule that hides #seo-prerender once #dc-root is populated.
// Idempotent: re-run after content changes. Needs the local server on :8765.
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const PW = '/Users/robintrassard/RustroverProjects/perf-sentinel/crates/sentinel-cli/tests/browser/node_modules/playwright';
const { chromium } = require(PW);

const SITE = join(dirname(fileURLToPath(import.meta.url)), '..');
const PAGES = [
  { file: 'index.html', url: 'http://127.0.0.1:8765/' },
  { file: 'guide.html', url: 'http://127.0.0.1:8765/guide' },
];

const CSS = '<style id="seo-prerender-css">x-dc{display:none}#dc-root:not(:empty)~#seo-prerender{display:none}</style>';
const START = '<!--seo-prerender-start-->';
const END = '<!--seo-prerender-end-->';

const browser = await chromium.launch();
for (const p of PAGES) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  await page.goto(p.url, { waitUntil: 'load' });
  await page.evaluate(() => { try { localStorage.clear(); } catch (e) {} });   // default lang (en) + default theme
  await page.reload({ waitUntil: 'load' });
  await page.waitForFunction(
    () => { const r = document.getElementById('dc-root'); return r && r.children.length && !/\{\{/.test(r.textContent); },
    { timeout: 20000 });
  await page.waitForTimeout(600);
  const html = await page.evaluate(() => {
    const clone = document.getElementById('dc-root').cloneNode(true);
    clone.querySelectorAll('script, iframe').forEach((e) => e.remove());
    return clone.innerHTML;
  });
  await page.close();

  let s = readFileSync(join(SITE, p.file), 'utf8');
  // strip any prior injection (idempotent)
  s = s.replace(/<style id="seo-prerender-css">[\s\S]*?<\/style>/g, '');
  s = s.replace(new RegExp(START + '[\\s\\S]*?' + END, 'g'), '');
  // inject CSS before </head>, block right after </x-dc>
  s = s.replace('</head>', CSS + '</head>');
  s = s.replace('</x-dc>', '</x-dc>' + START + '<div id="seo-prerender">' + html + '</div>' + END);
  writeFileSync(join(SITE, p.file), s);
  console.log('  ' + p.file + ': prerender ' + Math.round(html.length / 1024) + ' KB injected');
}
await browser.close();
console.log('OK prerender (home + guide)');
