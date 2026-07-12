#!/usr/bin/env node
// Regenerate the reference/** documentation pages + search indexes from the
// Markdown source in the main repo. This is the source of truth: editing a
// docs/*.md and re-running this reproduces the static Reference, with every
// chrome fix baked in (no postprocess step needed) and heading slugs deduped.
//
//   node scripts/build-reference.mjs
//
// Inputs:
//   - Markdown:   <MAIN_REPO>/docs/{id}.md (EN), docs/FR/{id}-FR.md (FR)
//   - Renderer:   ./mdrender.js  (window.PSMD.render)
//   - Frame:      ./reference/00-INDEX.html  (the <style> and inline <script>
//                 blocks are lifted verbatim, so all 6 chrome fixes ride along)
// Outputs: reference/{flat}.html, reference/fr/{flat}.html, and the two search.json.
import { readFileSync, writeFileSync, existsSync, mkdirSync, rmSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const SITE = join(dirname(fileURLToPath(import.meta.url)), '..');
const MAIN = process.env.SITE_MAIN_REPO || '/Users/robintrassard/RustroverProjects/perf-sentinel';
const REF = join(SITE, 'docs');
const SITE_DOCS = join(SITE, 'docs-site');

// --- doc registry + labels (mirrors Docs.dc.html registry()/groupLabel()/docLabel()) ---
const REGISTRY = [
  { key: 'overview', items: ['00-INDEX'] },
  { key: 'start', items: ['ARCHITECTURE', 'INSTRUMENTATION', 'CI'] },
  { key: 'deploy', items: ['INTEGRATION', 'HELM-DEPLOYMENT'] },
  { key: 'reference', items: ['CONFIGURATION', 'CLI', 'METRICS', 'QUERY-API', 'SARIF', 'SCHEMA'] },
  { key: 'features', items: ['HTML-REPORT', 'INSPECT', 'ACKNOWLEDGMENTS', 'ACK-WORKFLOW', 'REPORTING'] },
  { key: 'ops', items: ['RUNBOOK', 'METHODOLOGY', 'LIMITATIONS'] },
  { key: 'supply', items: ['SUPPLY-CHAIN', 'RELEASE-PROCEDURE'] },
  { key: 'design', items: ['design/00-INDEX', 'design/01-PIPELINE-AND-TYPES', 'design/02-NORMALIZATION', 'design/03-CORRELATION-AND-STREAMING', 'design/04-DETECTION', 'design/05-GREENOPS-AND-CARBON', 'design/06-INGESTION-AND-DAEMON', 'design/07-CLI-CONFIG-RELEASE', 'design/08-PERIODIC-DISCLOSURE', 'design/09-CARBON-ATTRIBUTION', 'design/10-SIGSTORE-ATTESTATION'] },
];
const GROUP_LABEL = {
  en: { overview: 'Overview', start: 'Getting started', deploy: 'Deployment', reference: 'Reference', features: 'Features', ops: 'Operations', supply: 'Supply chain & release', design: 'Design (contributors)' },
  fr: { overview: 'Général', start: 'Prise en main', deploy: 'Déploiement', reference: 'Référence', features: 'Fonctionnalités', ops: 'Exploitation', supply: 'Chaîne & release', design: 'Design (contributeurs)' },
};
const DOC_LABEL_EN = {
  '00-INDEX': 'Overview', 'ARCHITECTURE': 'Architecture', 'INTEGRATION': 'Integration', 'INSTRUMENTATION': 'Instrumentation', 'CI': 'CI',
  'CONFIGURATION': 'Configuration', 'CLI': 'CLI reference', 'METRICS': 'Metrics', 'QUERY-API': 'Query API', 'SARIF': 'SARIF', 'SCHEMA': 'Schema',
  'HTML-REPORT': 'HTML report', 'INSPECT': 'Inspect (TUI)', 'ACKNOWLEDGMENTS': 'Acknowledgments', 'ACK-WORKFLOW': 'Ack workflow', 'REPORTING': 'Reporting',
  'HELM-DEPLOYMENT': 'Helm deployment', 'RUNBOOK': 'Runbook', 'METHODOLOGY': 'Methodology', 'LIMITATIONS': 'Limitations',
  'SUPPLY-CHAIN': 'Supply chain', 'RELEASE-PROCEDURE': 'Release procedure',
  'design/00-INDEX': 'Design index', 'design/01-PIPELINE-AND-TYPES': '01 · Pipeline & types', 'design/02-NORMALIZATION': '02 · Normalization', 'design/03-CORRELATION-AND-STREAMING': '03 · Correlation & streaming', 'design/04-DETECTION': '04 · Detection', 'design/05-GREENOPS-AND-CARBON': '05 · GreenOps & carbon', 'design/06-INGESTION-AND-DAEMON': '06 · Ingestion & daemon', 'design/07-CLI-CONFIG-RELEASE': '07 · CLI, config & release', 'design/08-PERIODIC-DISCLOSURE': '08 · Periodic disclosure', 'design/09-CARBON-ATTRIBUTION': '09 · Carbon attribution', 'design/10-SIGSTORE-ATTESTATION': '10 · Sigstore & SLSA',
};
const DOC_LABEL_FR = {
  'design/09-CARBON-ATTRIBUTION': '09 · Attribution carbone',
  '00-INDEX': 'Vue d’ensemble', 'INTEGRATION': 'Intégration', 'CLI': 'Référence CLI', 'METRICS': 'Métriques', 'QUERY-API': 'API de query', 'SCHEMA': 'Schéma',
  'HTML-REPORT': 'Rapport HTML', 'ACKNOWLEDGMENTS': 'Acquittements', 'ACK-WORKFLOW': 'Flux d’acquittement', 'REPORTING': 'Divulgation',
  'HELM-DEPLOYMENT': 'Déploiement Helm', 'METHODOLOGY': 'Méthodologie', 'LIMITATIONS': 'Limites', 'SUPPLY-CHAIN': 'Chaîne d’appro.', 'RELEASE-PROCEDURE': 'Procédure de release',
  'design/00-INDEX': 'Index design', 'design/02-NORMALIZATION': '02 · Normalisation', 'design/04-DETECTION': '04 · Détection', 'design/08-PERIODIC-DISCLOSURE': '08 · Divulgation périodique',
};
const docLabel = (id, lang) => (lang === 'fr' && DOC_LABEL_FR[id]) || DOC_LABEL_EN[id] || id;
const groupLabel = (key, lang) => GROUP_LABEL[lang][key] || key;
const flat = (id) => id;
const hrefFor = (id, lang) => {
  const base = '/docs/' + (lang === 'fr' ? 'fr/' : '');
  if (id === '00-INDEX') return base;
  if (id === 'design/00-INDEX') return base + 'design/';
  return base + id;
};
const groupOf = (id) => (REGISTRY.find((g) => g.items.includes(id)) || REGISTRY[0]).key;
const ALL_IDS = REGISTRY.flatMap((g) => g.items);
const ID_SET = new Set(ALL_IDS);
const DESIGN_BASENAME = new Map(ALL_IDS.filter((id) => id.startsWith('design/')).map((id) => [id.slice('design/'.length), id]));

// Markdown source for a page: a curated site override (docs-site/) wins over the
// repo doc, so the directory-index pages can read as website copy without
// editing the repo (where the directory description is legitimate).
const mdSource = (id, lang) => {
  const override = join(SITE_DOCS, lang === 'fr' ? `${id}-FR.md` : `${id}.md`);
  return existsSync(override) ? override : (lang === 'fr' ? join(MAIN, 'docs/FR', `${id}-FR.md`) : join(MAIN, 'docs', `${id}.md`));
};

// Map a repo doc path to a registry id, or null for non-doc paths (code dirs,
// schemas/, .rs files, README.md, ...). Only fires on strings containing ".md".
function pathToId(p) {
  if (!/\.md(?:#|$)/.test(p)) return null;
  const s = p.replace(/#.*$/, '').replace(/^(\.\.?\/)+/, '')
    .replace(/^docs\/FR\//, '').replace(/^docs\//, '').replace(/^FR\//, '')
    .replace(/\.md$/, '').replace(/-FR$/, '');
  if (ID_SET.has(s)) return s;
  if (DESIGN_BASENAME.has(s)) return `design/${s}`;
  return null;
}

// per-language UI strings baked into the static chrome
const UI = {
  en: { navHome: 'Overview', navGuide: 'Guide', search: 'Search…', onPage: 'On this page', switchLabel: 'FR',
    ft: { tagline: 'Protocol-level I/O anti-pattern detector, self-hostable and carbon-aware.', product: 'Product', docs: 'Documentation', project: 'Project', detection: 'Detection', execution: 'Execution', perf: 'Performance', greenops: 'GreenOps', comparison: 'Comparison', quickstart: 'Quickstart', cli: 'CLI reference', config: 'Configuration', method: 'GreenOps methodology', license: 'AGPL-3.0 license', copyright: '© 2026 perf-sentinel · AGPL-3.0 · Eco-designed page, no tracking script', legal: 'Legal notice', privacy: 'Privacy', credit: 'Logo & banner, ' } },
  fr: { navHome: 'Vue d’ensemble', navGuide: 'Guide', search: 'Rechercher…', onPage: 'Sur cette page', switchLabel: 'EN',
    ft: { tagline: 'Détecteur d’anti-patterns d’I/O au niveau protocole, auto-hébergeable et carbon-aware.', product: 'Produit', docs: 'Documentation', project: 'Projet', detection: 'Détection', execution: 'Exécution', perf: 'Performance', greenops: 'GreenOps', comparison: 'Comparatif', quickstart: 'Démarrage rapide', cli: 'Référence CLI', config: 'Configuration', method: 'Méthodologie GreenOps', license: 'Licence AGPL-3.0', copyright: '© 2026 perf-sentinel · AGPL-3.0 · Page éco-conçue, sans script de tracking', legal: 'Mentions légales', privacy: 'Confidentialité', credit: 'Logo & bannière, ' } },
};

// --- renderer ---
const PSMD = new Function('window', 'document', readFileSync(join(SITE, 'mdrender.js'), 'utf8') + ';return window.PSMD;')({}, { createElement: () => ({}) });

// --- lift the verbatim frame blocks (carry the 6 chrome fixes) from current pages.
// The <style> is shared; the inline <script> is localized (the search "No results"
// string), so lift it per language from an EN and an FR page. ---
const grab = (file, re) => readFileSync(join(REF, file), 'utf8').match(re)[0];
// The frame style predates the globe language button: drop the old pill's
// aria-label hover rules and inject the .ps-lang-btn block (idempotent, so
// re-runs that grab an already-updated frame stay a no-op).
const LANG_CSS = ".ps-lang-btn{display:inline-flex;align-items:center;gap:6px;height:34px;box-sizing:border-box;padding:0 12px;border-radius:999px;border:1px solid var(--border);background:var(--surface);color:var(--text-2);text-decoration:none;cursor:pointer;font-family:'JetBrains Mono',monospace;font-size:11.5px;font-weight:600;line-height:1;transition:border-color .25s ease}.ps-lang-btn:hover,.ps-lang-btn:focus-visible{border-color:var(--accent)}.ps-lang-btn .ps-lang-ico{display:flex;color:var(--accent);transition:transform .45s cubic-bezier(.34,1.4,.5,1)}.ps-lang-btn:hover .ps-lang-ico,.ps-lang-btn:focus-visible .ps-lang-ico{transform:rotate(18deg)}.ps-lang-win{position:relative;display:block;width:19px;height:15px;overflow:hidden}.ps-lang-track{display:flex;width:38px;transition:transform .34s cubic-bezier(.5,0,.15,1)}.ps-lang-btn:hover .ps-lang-track,.ps-lang-btn:focus-visible .ps-lang-track{transform:translateX(-19px)}.ps-lang-cell{flex:0 0 19px;width:19px;height:15px;display:flex;align-items:center;justify-content:flex-start}.ps-lang-cur{color:var(--text-2)}.ps-lang-next{color:var(--text)}";
const RAW_STYLE = grab('00-INDEX.html', /<style>[\s\S]*?<\/style>/)
  .replace('header [aria-label="Language"],header [aria-label="Theme"]{', 'header [aria-label="Theme"]{')
  .replace('header [aria-label="Language"]:hover,header [aria-label="Theme"]:hover{', 'header [aria-label="Theme"]:hover{')
  // Align the dark-mode accent to the vitrine (#0BA671); the frame drifted to
  // the brighter terminal green (#26CD94). --term-accent stays untouched.
  .replace('--accent:#26CD94; --accent-strong:#26CD94;', '--accent:#0BA671; --accent-strong:#0BA671;');
const STYLE = RAW_STYLE.includes('.ps-lang-btn{') ? RAW_STYLE : RAW_STYLE.replace('</style>', LANG_CSS + '</style>');
// The EN and FR inline scripts differ only in the search "No results" string,
// so derive FR from EN (avoids reading our own output for the FR frame).
const SCRIPT_EN = grab('00-INDEX.html', /<script>[\s\S]*?<\/script>/);
const SCRIPT = { en: SCRIPT_EN, fr: SCRIPT_EN.replace('No results', 'Aucun résultat') };

// Static default for the theme pill: system mode (the inline script re-syncs at load).
const ICON_SYSTEM = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="13" rx="2"></rect><path d="M8 21h8M12 17v4"></path></svg>';
const GLOBE = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"></circle><path d="M3 12h18"></path><path d="M12 3c2.5 2.4 4 5.6 4 9s-1.5 6.6-4 9c-2.5-2.4-4-5.6-4-9s1.5-6.6 4-9z"></path></svg>';
// Language control: globe + horizontal-slide reveal, an <a> navigating to the
// same doc in the other language (active label at rest, target on hover).
const langCtl = (href, fr, target) => {
  const active = fr ? 'FR' : 'EN', aria = fr ? 'Changer de langue' : 'Switch language';
  return `<a data-plain data-lang-switch href="${href}" class="ps-lang-btn" aria-label="${aria}" title="${aria}"><span class="ps-lang-ico">${GLOBE}</span><span class="ps-lang-win"><span class="ps-lang-track"><span class="ps-lang-cell ps-lang-cur">${active}</span><span class="ps-lang-cell ps-lang-next">${target}</span></span></span></a>`;
};

// --- helpers ---
const plaintext = (html) =>
  html.replace(/<[^>]+>/g, ' ')
    .replace(/&amp;/g, '&').replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&#39;/g, "'").replace(/&quot;/g, '"')
    .replace(/\s+/g, ' ').trim();

// PSMD emits cross-doc links as href="#/{ID}" data-doc="{ID}" data-anchor="{A}".
// Static pages use href="{flat}#{A}" (or "{flat}", or "/reference/[fr/]" for the index)
// with data-* dropped. Files stay X.html (GitHub Pages serves them at the extensionless path).
function rewriteContentLinks(html, lang) {
  return html.replace(/<a href="#\/([^"]*)" data-doc="[^"]*"(?: data-anchor="([^"]*)")?>/g,
    (_m, id, anchor) => `<a href="${hrefFor(id, lang)}${anchor ? '#' + anchor : ''}">`);
}

// Repo doc paths leak into the prose as visible text: a markdown link whose text
// is the path, an inline-code path, or a bare path. Replace the visible path with
// the doc's label (and a working link); leave non-doc paths (code dirs, schemas).
function relabelDocPaths(html, lang) {
  // links whose visible text is a repo doc path -> show the doc label instead
  html = html.replace(/<a href="([^"]*)">([^<]*\.md(?:#[^<]*)?)<\/a>/g, (m, href, text) => {
    const id = pathToId(text.trim());
    return id ? `<a href="${href}">${docLabel(id, lang)}</a>` : m;
  });
  // inline-code or bare path text -> link to the doc, labelled. The code tags carry the
  // .ps-ic class; matching a bare <code> left the opening in place but consumed the real
  // </code>, leaving the span open so everything after it rendered mono. Match the real
  // opening, and never strip just one tag of the pair.
  html = html.replace(/(<code class="ps-ic">)?((?:\.{0,2}\/)?(?:docs\/)?(?:FR\/)?(?:design\/)?[A-Za-z0-9_.-]+\.md(?:#[\w.-]+)?)(<\/code>)?/g, (m, open, p, close) => {
    const id = pathToId(p);
    if (!id) return m;
    if (Boolean(open) !== Boolean(close)) return m;
    const anchor = (p.match(/#([\w.-]+)/) || [])[1];
    return `<a href="${hrefFor(id, lang)}${anchor ? '#' + anchor : ''}">${docLabel(id, lang)}</a>`;
  });
  return html;
}

// The mdrender slug() does not dedupe, so repeated headings emit duplicate ids.
// Suffix the 2nd+ occurrence (-2, -3, ...) in the content, and follow the rename
// through the toc entries and any in-content #anchor that targeted it (in order).
function dedupeSlugs(html, toc) {
  const seen = new Map();
  // rename heading ids in document order
  html = html.replace(/(<h[1-6] id=")([^"]*)(")/g, (m, pre, id, post) => {
    const n = (seen.get(id) || 0) + 1;
    seen.set(id, n);
    return n === 1 ? m : pre + id + '-' + n + post;
  });
  if ([...seen.values()].every((n) => n === 1)) return { html, toc }; // nothing duplicated
  // for each duplicated base id, the in-content anchor links and toc entries that
  // point at it must take the suffixed form for their 2nd+ occurrence, in order.
  const used = new Map();
  const next = (id) => { const n = (used.get(id) || 0) + 1; used.set(id, n); return n === 1 ? id : id + '-' + n; };
  // toc: data-anchor="{id}" / href="#{id}"
  const newToc = toc.map((t) => {
    if ((seen.get(t.id) || 0) <= 1) return t;
    return { ...t, id: next(t.id) };
  });
  // rebuild used counters for the html anchor pass (toc consumed them above only for dup ids)
  used.clear();
  html = html.replace(/(<a href="#)([^"]*)(" data-anchor=")([^"]*)(">)/g, (m, a, href, b, anchor, c) => {
    if ((seen.get(href) || 0) <= 1) return m;
    const r = next(href);
    return a + r + b + r + c;
  });
  return { html, toc: newToc };
}

const description = (html) => {
  const m = html.match(/<p class="ps-p">([\s\S]*?)<\/p>/);
  const t = m ? plaintext(m[1]) : '';
  return t.length > 152 ? t.slice(0, 152) + '…' : t;
};

// Match the original generator: attribute values escape only the quote that
// would close the attribute; &, <, > are left raw (HTML5-valid in "..." attrs).
const attr = (s) => s.replace(/"/g, '&quot;');

function sidebar(activeId, lang) {
  return REGISTRY.map((g) => {
    const open = groupOf(activeId) === g.key ? 'true' : 'false';
    const items = g.items.map((id) =>
      `<a href="${hrefFor(id, lang)}" data-active="${id === activeId ? 'true' : 'false'}">${docLabel(id, lang)}</a>`).join('');
    return `<button type="button" class="ps-group-btn" data-open="${open}"><span>${groupLabel(g.key, lang)}</span><span class="ps-chev">&#8250;</span></button><nav class="ps-docnav" data-open="${open}">${items}</nav>`;
  }).join('');
}

const tocHtml = (toc) =>
  toc.map((t) => `<a href="#${t.id}" data-anchor="${t.id}" data-lvl="${t.lvl}">${t.txt}</a>`).join('');

// The renderer bakes one _dark diagram variant (rendered with theme:'dark').
// Emit both and let the [data-theme] CSS that swaps the logo pick the right one.
function diagramThemePair(html) {
  return html.replace(
    /<img class="ps-diagram" src="([^"]+?)_dark\.svg"([^>]*)>/g,
    (_m, base, rest) =>
      `<img class="ps-diagram" data-diagram="light" src="${base}.svg"${rest}>` +
      `<img class="ps-diagram" data-diagram="dark" src="${base}_dark.svg"${rest}>`,
  );
}

function buildPage(id, lang) {
  const fr = lang === 'fr';
  const r = PSMD.render(readFileSync(mdSource(id, lang), 'utf8'), { id, lang, theme: 'dark', label: (x) => docLabel(x, lang) });
  const dd = dedupeSlugs(rewriteContentLinks(r.html, lang), r.toc);
  const content = diagramThemePair(relabelDocPaths(dd.html, lang));
  const toc = dd.toc;

  const label = docLabel(id, lang);
  const title = `${label} · perf-sentinel docs`;
  const desc = description(content);
  const f = flat(id);
  const enHref = hrefFor(id, 'en');
  const frHref = hrefFor(id, 'fr');
  const switchHref = hrefFor(id, fr ? 'en' : 'fr');
  const ui = UI[lang];
  const hasToc = toc.length > 1;

  const head =
    `<!DOCTYPE html><html lang="${lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">` +
    `<title>${title}</title><meta name="description" content="${attr(desc)}">` +
    `<link rel="canonical" href="${hrefFor(id, lang)}"><link rel="alternate" hreflang="en" href="${enHref}"><link rel="alternate" hreflang="fr" href="${frHref}">` +
    `<meta property="og:type" content="article"><meta property="og:title" content="${attr(title)}"><meta property="og:description" content="${attr(desc)}">` +
    `<meta property="og:image" content="https://perf-sentinel.dev/assets/og-banner.png"><meta property="og:image:width" content="1200"><meta property="og:image:height" content="427"><meta property="og:image:alt" content="perf-sentinel"><meta name="theme-color" content="#0BA671">` +
    `<meta name="twitter:card" content="summary_large_image"><meta name="twitter:image" content="https://perf-sentinel.dev/assets/og-banner.png"><link rel="icon" type="image/svg+xml" href="/assets/favicon.svg"><link rel="stylesheet" href="/fonts/fonts.css">${STYLE}</head>`;

  const header =
    `<header class="ps-chrome" style="position:sticky;top:0;z-index:50;background:color-mix(in srgb,var(--bg) 85%,transparent);backdrop-filter:blur(10px);border-bottom:1px solid var(--border)"><div style="max-width:1320px;margin:0 auto;padding:14px 28px;display:flex;align-items:center;gap:20px;position:relative"><input id="ps-navtoggle" type="checkbox" aria-hidden="true" style="display:none">` +
    `<a href="/" style="display:flex;align-items:center;flex:none"><img data-logo="light" src="/assets/logo-h-light.svg" alt="perf sentinel" style="height:38px;width:auto"><img data-logo="dark" src="/assets/logo-h-dark.svg" alt="perf sentinel" style="height:38px;width:auto"></a>` +
    `<span class="ps-hdr-badge" style="font-size:12px;color:var(--text-2);border:1px solid var(--border);border-radius:6px;padding:3px 9px">docs</span>` +
    `<nav class="ps-hdr-nav" style="display:flex;gap:20px;margin-left:6px;font-size:14.5px;font-weight:500;color:var(--text-2)"><a href="/">${ui.navHome} ↗</a><a href="/guide">${ui.navGuide} ↗</a></nav>` +
    `<div style="margin-left:auto;display:flex;align-items:center;gap:12px;font-family:'JetBrains Mono',monospace;font-size:12px">` +
    `<div id="psSearchWrap" style="position:relative"><input id="psSearch" type="search" placeholder="${ui.search}" autocomplete="off"><div id="psResults" class="psr" style="display:none"></div></div>` +
    langCtl(switchHref, fr, ui.switchLabel) +
    `<button id="themeBtn" class="ps-th-btn" aria-label="Theme"><span class="ps-th-ico">${ICON_SYSTEM}</span><span class="ps-th-lbl">${fr ? 'Système' : 'System'}</span></button>` +
    `<a href="https://github.com/robintra/perf-sentinel" aria-label="perf-sentinel on GitHub" style="display:flex;align-items:center;gap:8px;font-size:14px;font-weight:600;color:#fff;background:#24292F;border:1px solid rgba(240,246,252,.18);border-radius:8px;padding:8px 15px"><svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"></path></svg><span class="gh-txt">GitHub</span></a>` +
    `<label for="ps-navtoggle" data-burger="" aria-label="Menu"><i></i></label>` +
    `</div>` +
    `<nav data-mobnav="" aria-label="Menu"><a href="/">${ui.navHome} ↗</a><a href="/guide">${ui.navGuide} ↗</a></nav>` +
    `</div></header>`;

  const main =
    `<main class="ps-content" style="min-width:0;padding:40px 44px 96px"><div style="font-size:12.5px;color:var(--accent);letter-spacing:.03em;margin-bottom:18px">Documentation / ${label}</div><div class="ps-md">${content}</div></main>`;

  const tocCol = hasToc
    ? `<aside class="ps-toc-col ps-chrome" style="position:sticky;top:65px;align-self:start;height:calc(100vh - 65px);overflow-y:auto;padding:40px 28px 40px 8px"><div style="font-size:11px;letter-spacing:.07em;text-transform:uppercase;color:var(--text-2);padding:0 0 12px 12px">${ui.onPage}</div><nav class="ps-toc">${tocHtml(toc)}</nav></aside>`
    : '';

  const asideSidebar = `<aside class="ps-aside ps-chrome" style="position:sticky;top:65px;align-self:start;height:calc(100vh - 65px);overflow-y:auto;border-right:1px solid var(--border);padding:26px 16px 40px 28px">${sidebar(id, lang)}</aside>`;

  const L = ui.ft;
  const ftCol = (t, links) => `<div><div style="font-size:13px;font-weight:600;color:var(--text);margin-bottom:13px">${t}</div><div style="display:flex;flex-direction:column;gap:9px;font-size:14px;color:var(--text-2)">${links.map(([h, x]) => `<a href="${h}">${x}</a>`).join('')}</div></div>`;
  const footer =
    `<footer style="border-top:1px solid var(--border);background:var(--surface-2)"><div style="max-width:1320px;margin:0 auto;padding:48px 28px 36px">` +
    `<div data-grid="ftcols" style="display:grid;grid-template-columns:1.5fr 1fr 1fr 1fr;gap:32px">` +
    `<div><img data-logo="light" src="/assets/logo-h-light.svg" alt="perf sentinel" style="height:40px;width:auto"><img data-logo="dark" src="/assets/logo-h-dark.svg" alt="perf sentinel" style="height:40px;width:auto"><p style="margin:16px 0 0;font-size:13.5px;color:var(--text-2);max-width:240px;line-height:1.55">${L.tagline}</p></div>` +
    ftCol(L.product, [['/#detection', L.detection], ['/#modes', L.execution], ['/#perf', L.perf], ['/#greenops', L.greenops], ['/#comparatif', L.comparison]]) +
    ftCol(L.docs, [['/guide#quickstart', L.quickstart], ['/guide#cli', L.cli], ['/guide#config', L.config], ['/guide#metrics', L.method]]) +
    ftCol(L.project, [['https://github.com/robintra/perf-sentinel', 'GitHub'], ['https://crates.io/crates/perf-sentinel', 'crates.io'], ['https://docs.rs/perf-sentinel-core', 'docs.rs'], ['/#license', L.license]]) +
    `</div>` +
    `<div style="display:flex;justify-content:space-between;gap:18px;margin-top:26px;flex-wrap:wrap;font-size:12.5px;color:var(--text-2)">` +
    `<div style="display:flex;flex-direction:column;gap:6px"><span>${L.copyright}</span><span style="display:flex;gap:14px"><a href="${lang === 'fr' ? '/mentions-legales' : '/legal-notice'}">${L.legal}</a><a href="${lang === 'fr' ? '/confidentialite' : '/privacy-policy'}">${L.privacy}</a></span></div>` +
    `<span>${L.credit}<a href="https://www.linkedin.com/in/gwendoline-meignen-b0224873/" target="_blank" rel="noopener" id="ftCredit">Gwendoline Meignen</a></span>` +
    `</div></div></footer>`;

  return head +
    `<body><div data-ps-root data-theme="light"><div style="min-height:100vh;background-color:var(--bg);color:var(--text)">` +
    header +
    `<div class="ps-layout" style="max-width:1320px;margin:0 auto;display:grid;grid-template-columns:248px 1fr 232px;gap:0;align-items:start">` +
    asideSidebar + main + tocCol +
    `</div>` + footer + `</div></div>${SCRIPT[lang]}</body></html>`;
}

function searchIndex(lang) {
  return REGISTRY.flatMap((g) => g.items).map((id) => {
    const r = PSMD.render(readFileSync(mdSource(id, lang), 'utf8'), { id, lang, theme: 'dark', label: (x) => docLabel(x, lang) });
    const html = relabelDocPaths(rewriteContentLinks(r.html, lang), lang);
    const t = docLabel(id, lang);
    return { t, u: hrefFor(id, lang), x: (t + ' ' + plaintext(html)).slice(0, 2600) };
  });
}

// --- run ---
mkdirSync(join(REF, 'design'), { recursive: true });
mkdirSync(join(REF, 'fr', 'design'), { recursive: true });
for (const dir of [REF, join(REF, 'fr')]) for (const f of readdirSync(dir)) if (f.startsWith('design__')) rmSync(join(dir, f));
let n = 0;
for (const id of REGISTRY.flatMap((g) => g.items)) {
  writeFileSync(join(REF, `${flat(id)}.html`), buildPage(id, 'en')); n++;
  writeFileSync(join(REF, 'fr', `${flat(id)}.html`), buildPage(id, 'fr')); n++;
}
writeFileSync(join(REF, 'index.html'), buildPage('00-INDEX', 'en'));
writeFileSync(join(REF, 'fr', 'index.html'), buildPage('00-INDEX', 'fr'));
writeFileSync(join(REF, 'design', 'index.html'), buildPage('design/00-INDEX', 'en'));
writeFileSync(join(REF, 'fr', 'design', 'index.html'), buildPage('design/00-INDEX', 'fr'));
writeFileSync(join(REF, 'search.json'), JSON.stringify(searchIndex('en')));
writeFileSync(join(REF, 'fr', 'search.json'), JSON.stringify(searchIndex('fr')));
console.log(`wrote ${n} pages + 2 search indexes`);
