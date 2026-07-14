// Regenerates the embedded dashboard for the landing-page iframe and re-applies
// the embed patch, which is NOT part of the perf-sentinel report generator.
//
// `perf-sentinel demo --html` emits a standalone self-contained report (its CSS,
// including the narrow-viewport media query, is baked in by the generator). The
// landing page embeds it in an iframe (exemple/dashboard.html) and drives its
// theme live from the site theme toggle. That needs one injected inline <script>
// which: reads ?theme=light|dark into sessionStorage for the initial render,
// applies a same-origin postMessage {psTheme} live by setting <html data-theme>,
// and, only when framed, hides the dashboard's own theme and density toggles
// (the site owns the theme, and the preview is too small to spend a control on)
// and forces compact density so the preview fits more rows. Opening the report
// full-page keeps both toggles and the report's own comfort default.
//
// The patch goes right before </head>: the report's own density bootstrap sits
// in the head and would otherwise overwrite the forced compact. It must still
// run before the app IIFE at the end of body, which resolves the theme.
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
// `#density-toggle` is named on its own: it shares the .ps-theme-toggle class for
// styling, so the class alone would hide it only by accident.
const PATCH = `<script>try{var k='perf-sentinel:theme';var m=(location.search.match(/[?&]theme=(dark|light)(?:&|$)/)||[])[1];if(m)sessionStorage.setItem(k,m)}catch(e){}
if(window.self!==window.top){try{document.documentElement.setAttribute('data-density','compact')}catch(e){}
try{var ps=document.createElement('style');ps.textContent='.ps-theme-toggle,#density-toggle{display:none!important}';(document.head||document.documentElement).appendChild(ps)}catch(e){}}
window.addEventListener('message',function(e){if(e.origin!==location.origin)return;var t=e.data&&e.data.psTheme;if(t==='light'||t==='dark'){document.documentElement.setAttribute('data-theme',t);try{sessionStorage.setItem('perf-sentinel:theme',t)}catch(_){}}});</script>`;

console.log(`[embed-dashboard] generating exemple/dashboard.html via ${bin}`);
execFileSync(bin, ['demo', '--html', OUT], { stdio: 'inherit' });

let html = readFileSync(OUT, 'utf8');
if (html.includes('psTheme')) {
  console.log('[embed-dashboard] embed patch already present, skipping injection');
} else {
  // Guard the assumption the forced density rests on: the report must still
  // ship its own density bootstrap, and it must sit before the injection point.
  const densityBootstrap = html.indexOf("localStorage.getItem('perf-sentinel:density')");
  const headEnd = html.indexOf('</head>');
  if (headEnd === -1) {
    console.error('[embed-dashboard] </head> not found, cannot inject the embed patch');
    process.exit(1);
  }
  if (densityBootstrap === -1 || densityBootstrap > headEnd) {
    console.error('[embed-dashboard] the report no longer bootstraps density in <head>; the forced compact would be a no-op or would fight the report. Re-check the patch against the template.');
    process.exit(1);
  }
  html = `${html.slice(0, headEnd)}${PATCH}\n${html.slice(headEnd)}`;
  writeFileSync(OUT, html);
  console.log('[embed-dashboard] embed patch injected before </head>');
}
