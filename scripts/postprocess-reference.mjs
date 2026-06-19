#!/usr/bin/env node
// Re-apply the chrome fixes the static reference generator (Docs.dc.html logic)
// misses. Run after every regeneration of reference/** :  node scripts/postprocess-reference.mjs
// Idempotent: each fix only fires when its broken pattern is present, so running
// it twice is a no-op (every `from` is chosen so it never matches its own `to`).
// ponytail: a string patch, NOT a reimplemented generator. When/if the generator
// is built, fold these into it and delete this file.
import { readdirSync, statSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const REF = join(dirname(fileURLToPath(import.meta.url)), '..', 'reference');

const GH_ICON =
  '<svg viewBox="0 0 24 24" width="17" height="17" fill="currentColor" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"></path></svg>';

// Each fix: `from` = the exact broken substring the generator emits, `to` = the
// fix, `marker` = a short string the FIXED page must contain (drives the self-check
// so a generator change that silently bypasses a fix is caught, not just fix #1).
const FIXES = [
  // 1. Outer IIFE never closed -> SyntaxError kills ALL inline JS. Append `})();`.
  {
    name: 'iife-close',
    from: `display='none'});})();</script>`,
    to: `display='none'});})();})();</script>`,
    marker: `});})();})();</script>`,
  },
  // 2. Theme button is a static "Theme". Give it a dot + dynamic localized label.
  {
    name: 'theme-button',
    from: `<button id="themeBtn" aria-label="Theme" style="font-family:'JetBrains Mono',monospace;font-size:12px;color:var(--text-2);background:var(--surface);border:1px solid var(--border);border-radius:999px;padding:7px 13px;cursor:pointer">Theme</button>`,
    to: `<button id="themeBtn" aria-label="Theme" style="display:flex;align-items:center;gap:7px;font-family:'JetBrains Mono',monospace;font-size:12px;color:var(--text-2);background:var(--surface);border:1px solid var(--border);border-radius:999px;padding:7px 13px;cursor:pointer"><span style="width:9px;height:9px;border-radius:50%;background:var(--accent);display:inline-block"></span><span id="themeLbl">Theme</span></button>`,
    marker: `id="themeLbl"`,
  },
  // 3. Make setT() update the label (target theme), localized via <html lang>.
  {
    name: 'theme-setT',
    from: `function setT(t){R.setAttribute('data-theme',t);try{localStorage.setItem('ps-theme',t)}catch(e){}}`,
    to: `function setT(t){R.setAttribute('data-theme',t);try{localStorage.setItem('ps-theme',t)}catch(e){}var L=document.getElementById('themeLbl');if(L){var fr=document.documentElement.lang==='fr';L.textContent=t==='dark'?(fr?'Clair':'Light'):(fr?'Sombre':'Dark')}}`,
    marker: `L.textContent=t===`,
  },
  // 4. GitHub header link is missing its octocat icon (the flex `gap:8px` expects one).
  {
    name: 'github-icon',
    from: `<a href="https://github.com/robintra/perf-sentinel" style="display:flex;align-items:center;gap:8px;font-size:14px;font-weight:600;color:#fff;background:#24292F;border-radius:8px;padding:8px 15px">GitHub</a>`,
    to: `<a href="https://github.com/robintra/perf-sentinel" aria-label="perf-sentinel on GitHub" style="display:flex;align-items:center;gap:8px;font-size:14px;font-weight:600;color:#fff;background:#24292F;border-radius:8px;padding:8px 15px">${GH_ICON}GitHub</a>`,
    marker: `aria-label="perf-sentinel on GitHub"`,
  },
  // 5. Search esc() escapes only & < > ; extend it to quotes so it is safe in
  //    attribute context too.
  {
    name: 'esc-quotes',
    from: `function esc(s){return s.replace(/[&<>]/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;'}[c]})}`,
    to: `function esc(s){return s.replace(/[&<>"']/g,function(c){return{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]})}`,
    marker: `/[&<>"']/g`,
  },
  // 6. The result URL `d.u` was interpolated into href="" unescaped. Run it through esc().
  {
    name: 'href-esc',
    from: `'<a href="'+d.u+'"><div class="psr-t">'`,
    to: `'<a href="'+esc(d.u)+'"><div class="psr-t">'`,
    marker: `'<a href="'+esc(d.u)+'"`,
  },
];

if (!existsSync(REF)) {
  console.error(`reference/ not found at ${REF} — run after the reference pages are generated, from the site root.`);
  process.exit(1);
}

const htmlFiles = (dir) =>
  readdirSync(dir).flatMap((n) => {
    const p = join(dir, n);
    return statSync(p).isDirectory() ? htmlFiles(p) : p.endsWith('.html') ? [p] : [];
  });

const files = htmlFiles(REF);
const applied = {};
let touched = 0;
for (const file of files) {
  const before = readFileSync(file, 'utf8');
  let html = before;
  for (const fix of FIXES) {
    if (html.includes(fix.from)) {
      html = html.replaceAll(fix.from, fix.to);
      applied[fix.name] = (applied[fix.name] || 0) + 1;
    }
  }
  if (html !== before) {
    writeFileSync(file, html);
    touched++;
  }
}

console.log(`reference pages: ${files.length}, patched this run: ${touched}`);
for (const [name, n] of Object.entries(applied)) console.log(`  ${name}: ${n}`);

// Self-check: every page must carry every fix's marker. Catches a generator
// change that makes a `from` stop matching (the fix silently no-ops otherwise).
const missing = [];
for (const file of files) {
  const html = readFileSync(file, 'utf8');
  for (const fix of FIXES) {
    if (!html.includes(fix.marker)) missing.push(`${file}: ${fix.name}`);
  }
}
if (missing.length) {
  console.error(`FAIL: ${missing.length} page/fix marker(s) missing — a fix did not apply:`);
  for (const m of missing.slice(0, 12)) console.error(`  ${m}`);
  process.exit(1);
}
console.log(`self-check OK: all ${FIXES.length} fixes present on all ${files.length} pages`);
