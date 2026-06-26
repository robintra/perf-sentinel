// Regenerates the embedded dashboard for the landing-page iframe and re-applies
// the theme-sync patch, which is NOT part of the perf-sentinel report generator.
//
// `perf-sentinel demo --html` emits a standalone self-contained report (its CSS,
// including the narrow-viewport media query, is baked in by the generator). The
// landing page embeds it in an iframe (exemple/dashboard.html) and drives its
// theme live from the site theme toggle. That needs one injected inline <script>
// (right after the CSP meta) which: reads ?theme=light|dark into sessionStorage
// for the initial render, hides the dashboard's own theme toggle when embedded,
// and applies a same-origin postMessage {psTheme} live by setting <html
// data-theme>. The generator does not emit this (it is site-specific), so a
// fresh `demo --html` drops it. Run this after regenerating to keep the iframe
// theme sync working (index.html / fr/index.html post the theme to the frame).
//
// Run: node scripts/build-embed-dashboard.mjs
// Binary resolution: $PERF_SENTINEL, else ../perf-sentinel/target/release/perf-sentinel,
// else `perf-sentinel` on PATH. Idempotent: skips injection if already patched.
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const OUT = join(ROOT, 'exemple/dashboard.html');
const SIBLING = resolve(ROOT, '../perf-sentinel/target/release/perf-sentinel');
const bin = process.env.PERF_SENTINEL || (existsSync(SIBLING) ? SIBLING : 'perf-sentinel');

// Keep in lockstep with the embed/theme-sync logic in index.html + fr/index.html.
const PATCH = `<script>try{var k='perf-sentinel:theme';var m=(location.search.match(/[?&]theme=(dark|light)(?:&|$)/)||[])[1];if(m)sessionStorage.setItem(k,m)}catch(e){}
if(window.self!==window.top){try{var ps=document.createElement('style');ps.textContent='.ps-theme-toggle{display:none!important}';(document.head||document.documentElement).appendChild(ps)}catch(e){}}
window.addEventListener('message',function(e){if(e.origin!==location.origin)return;var t=e.data&&e.data.psTheme;if(t==='light'||t==='dark'){document.documentElement.setAttribute('data-theme',t);try{sessionStorage.setItem('perf-sentinel:theme',t)}catch(_){}}});</script>`;

console.log(`[embed-dashboard] generating exemple/dashboard.html via ${bin}`);
execFileSync(bin, ['demo', '--html', OUT], { stdio: 'inherit' });

let html = readFileSync(OUT, 'utf8');
if (html.includes('psTheme')) {
  console.log('[embed-dashboard] theme-sync patch already present, skipping injection');
} else {
  const before = html;
  html = html.replace(/<meta http-equiv="Content-Security-Policy"[^>]*>/, (m) => `${m}\n${PATCH}`);
  if (html === before) {
    console.error('[embed-dashboard] CSP meta not found, cannot inject the theme-sync patch');
    process.exit(1);
  }
  writeFileSync(OUT, html);
  console.log('[embed-dashboard] theme-sync patch injected after the CSP meta');
}
