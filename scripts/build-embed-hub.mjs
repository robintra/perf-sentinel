// Regenerates the embedded Hub launcher for the landing-page iframe and applies
// the embed patch, which is NOT part of PerfSentinelHub.
//
// The launcher is already a static page: nine files, no build step, fonts base64
// in fonts.css, icons inline SVG, and no request outside the Hub's own origin. It
// never asks the server how to draw, only what to draw, and every read goes
// through one helper (getJson in app.js). So the whole interface renders from a
// replaced window.fetch, with no fork of the launcher and no second copy of its
// copy, thresholds or colour bands.
//
// What the patch must do beyond answering, and why a table of constants is not
// enough: the page is alive. It re-reads a daemon row on a ticker, counts down to
// the next read, prints "Read N s ago", polls a submitted run every second, and
// its gauge badges are built for an uptime that grows every read. Constants would
// freeze the gauges while the labels ticked, which reads as a broken page. So the
// patch rebases every captured timestamp onto load time, walks the gauges between
// reads, and carries a submitted run through pending, running and succeeded.
//
// Run: node scripts/build-embed-hub.mjs
// Hub checkout: $PERF_SENTINEL_HUB, else ../../RiderProjects/PerfSentinelHub.
// Fixtures come from that checkout's tests/browser/demo/fixtures/hub-embed.json,
// written by its Playwright global-setup against a real populated Hub.
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const OUT_DIR = join(ROOT, 'exemple/hub');
const SIBLING = resolve(ROOT, '../../RiderProjects/PerfSentinelHub');
const HUB = process.env.PERF_SENTINEL_HUB || SIBLING;
const WWWROOT = join(HUB, 'PerfSentinelHub/wwwroot');
const FIXTURES = join(HUB, 'tests/browser/demo/fixtures/hub-embed.json');

const die = (message) => {
  console.error(`[embed-hub] ${message}`);
  process.exit(1);
};

if (!existsSync(WWWROOT)) die(`no launcher at ${WWWROOT}. Set PERF_SENTINEL_HUB to a Hub checkout.`);
if (!existsSync(FIXTURES)) {
  die(`no fixtures at ${FIXTURES}. Run the Hub's browser demo suite once to record them.`);
}

const fixtures = JSON.parse(readFileSync(FIXTURES, 'utf8'));
// Guard the fixture shape rather than discovering a missing route as an empty
// screen in the browser. /api/status is the hard gate: without it the launcher
// renders one red banner and nothing else.
const required = ['/api/status', '/api/sources', '/api/analyses?limit=500'];
for (const route of required) {
  if (!fixtures.routes || !(route in fixtures.routes)) die(`fixtures carry no ${route}`);
}
if (typeof fixtures.epoch_ms !== 'number') die('fixtures carry no epoch_ms, so nothing can be rebased');
const daemonRoutes = Object.keys(fixtures.routes).filter((r) => r.endsWith('/daemon'));
if (daemonRoutes.length === 0) die('fixtures carry no daemon view, so the fleet rows would not unfold');
// Every state of a submitted run is derived from a captured succeeded one, so
// without one the submit button would lead to a screen built from nothing.
const succeeded = Object.entries(fixtures.routes).some(
  ([route, body]) => route.startsWith('/api/analyses/') && body && body.status === 'succeeded');
if (!succeeded) die('fixtures carry no succeeded run, so a submitted run would have no shape to take');

const source = readFileSync(join(WWWROOT, 'index.html'), 'utf8');
const app = readFileSync(join(WWWROOT, 'app.js'), 'utf8');
const tokens = readFileSync(join(WWWROOT, 'tokens.css'), 'utf8');

// Three assumptions this patch rests on, each of which fails loudly rather than
// producing a page that looks fine and is not.
const headEnd = source.indexOf('</head>');
if (headEnd === -1) die('</head> not found in the launcher, cannot inject the patch');
if (!app.includes('function getJson(')) {
  die('app.js no longer routes its reads through getJson; replacing fetch may no longer cover them. Re-check the patch against the launcher.');
}
// The patch overrides a resolution that has already happened, so it depends on
// that resolution still being the blocking head script it overrides. If the
// launcher moved it, the override could fight it or arrive first and be undone.
const themeBootstrap = source.indexOf('document.documentElement.dataset.theme');
if (themeBootstrap === -1 || themeBootstrap > headEnd) {
  die('the launcher no longer resolves its theme in a blocking <head> script; the patch that overrides it may now run first and be overwritten. Re-check the patch against the launcher.');
}
if (!tokens.includes('[data-theme="dark"]') || !tokens.includes('[data-theme="light"]')) {
  die('tokens.css no longer keys its palettes off data-theme; setting that attribute would no longer change anything.');
}
// The framed copy hides the launcher's own theme control. Its label is drawn
// from application state this patch cannot reach, so left visible it would read
// "Dark" over a page the site had just turned light.
if (!source.includes('id="theme-toggle"')) {
  die('the launcher no longer carries #theme-toggle; the framed stylesheet would hide nothing and the embed would ship a stale theme control. Re-check the patch against the launcher.');
}

const PATCH = `<script>
(function () {
  var F = ${JSON.stringify(fixtures).replace(/</g, '\\u003c')};
  var ROUTES = F.routes;
  // A captured moment is stored against a fixed epoch, so one offset moves the
  // whole set onto now and every gap between them survives. Without this a run
  // captured today reads as eight months old next spring.
  //
  // The snapshot is also aged deliberately. Capture happens seconds after the
  // Hub's first poll, so every row would read "1 s ago" and the fleet would look
  // like it had just booted. Ageing the whole set by one offset keeps every gap
  // intact and puts the reader in front of a settled fleet, the same reason
  // capture-fixtures.sh chooses gauge values rather than recording an idle
  // daemon's zeros.
  var AGED_BY_MS = 8 * 60 * 1000;
  // Above this a number is a moment, below it a duration. Kept in step with
  // IS_TIMESTAMP in the Hub's global-setup.ts, which decides the same thing on
  // the way in.
  var IS_TIMESTAMP = 1e12;
  var LOAD = Date.now();
  var DELTA = LOAD - F.epoch_ms - AGED_BY_MS;

  function shift(value) {
    if (Array.isArray(value)) return value.map(shift);
    if (value && typeof value === 'object') {
      var out = {};
      for (var k in value) {
        if (!Object.prototype.hasOwnProperty.call(value, k)) continue;
        var v = value[k];
        out[k] = (k.slice(-3) === '_ms' && typeof v === 'number' && v > IS_TIMESTAMP) ? v + DELTA : shift(v);
      }
      return out;
    }
    return value;
  }

  var reads = {};
  // A small closed walk rather than randomness, so two visitors see the same
  // page and a screenshot of it is reproducible. Every gauge on this screen
  // counts toward a cap, so the walk stays well inside its band.
  var WALK = [0, 7, -3, 11, -5, 4, -8, 2];

  // pct is a percentage rounded to one decimal, and at_capacity is the engine's
  // own advisor line at 90 %, not a full gauge. Recomputing them the way
  // DaemonView.cs does keeps a walked value toned exactly as a real read.
  function gauge(g, step) {
    if (!g || typeof g.value !== 'number' || typeof g.capacity !== 'number') return g;
    var value = Math.max(0, Math.min(g.capacity, g.value + step));
    var pct = Math.max(0, Math.min(100, value / g.capacity * 100));
    return { value: value, capacity: g.capacity, pct: Math.round(pct * 10) / 10,
             at_capacity: pct >= 90 };
  }

  function daemonView(path) {
    var body = shift(ROUTES[path]);
    var n = reads[path] = (reads[path] || 0) + 1;
    var step = WALK[(n - 1) % WALK.length];
    body.traces = gauge(body.traces, step);
    body.analysis_queue = gauge(body.analysis_queue, Math.round(step / 3));
    body.findings = gauge(body.findings, n % 3 === 0 ? 1 : 0);
    // Uptime follows the wall clock rather than the read count, so it stays
    // truthful whether the reader refreshes every five seconds or leaves the
    // row open.
    if (typeof body.uptime_seconds === 'number') {
      body.uptime_seconds += Math.floor((Date.now() - LOAD) / 1000);
    }
    body.observed_at_ms = Date.now();
    return body;
  }

  // A submitted run is carried through the states the launcher's own poll
  // expects, reusing a real succeeded run for the terminal body.
  var submitted = {};
  var submits = 0;
  var SUBMIT_HISTORY = 20;
  var TEMPLATE = null;
  for (var key in ROUTES) {
    if (key.indexOf('/api/analyses/') === 0 && ROUTES[key] && ROUTES[key].status === 'succeeded') {
      TEMPLATE = ROUTES[key];
      break;
    }
  }

  var SOURCES = ROUTES['/api/sources'] || [];
  function sourceOf(id) {
    for (var i = 0; i < SOURCES.length; i++) if (SOURCES[i].id === id) return SOURCES[i];
    return null;
  }

  // Every state is derived from the captured succeeded run rather than built by
  // hand. A run carries fifteen fields and the screens read most of them, so a
  // hand-built pending body prints "read from undefined" for the one it forgot.
  function run(id) {
    var started = submitted[id];
    var age = Date.now() - started.at;
    var status = age < 1500 ? 'pending' : (age < 4000 ? 'running' : 'succeeded');
    var body = shift(TEMPLATE);
    var src = sourceOf(started.sourceId);
    body.id = id;
    body.status = status;
    body.created_at_ms = started.at;
    body.started_at_ms = status === 'pending' ? null : started.at + 1500;
    body.finished_at_ms = status === 'succeeded' ? started.at + 4000 : null;
    body.request = started.request;
    if (src) {
      body.source_id = src.id;
      body.source_name = src.name;
      body.environment = src.environment;
      body.kind = src.kind;
      body.producer_version = status === 'succeeded' ? src.producer_version : null;
    }
    if (status !== 'succeeded') {
      body.result = null;
      body.expires_at_ms = null;
    }
    return body;
  }

  function json(body) {
    return Promise.resolve(new Response(JSON.stringify(body), {
      status: 200, headers: { 'content-type': 'application/json' }
    }));
  }

  var realFetch = window.fetch.bind(window);
  window.fetch = function (input, init) {
    var url = typeof input === 'string' ? input : (input && input.url) || '';
    var target;
    try { target = new URL(url, location.href); } catch (e) { return realFetch(input, init); }
    // Same origin only. Stripping any host would let a request to another origin
    // be answered from a fixture whose path happened to match.
    if (target.origin !== location.origin) return realFetch(input, init);
    var path = target.pathname + target.search;
    var method = ((init && init.method) || 'GET').toUpperCase();

    if (method === 'POST' && path.indexOf('/api/analyses') === 0) {
      var payload = {};
      try { payload = JSON.parse((init && init.body) || '{}'); } catch (e) {}
      var id = 'f' + String(++submits).padStart(15, '0');
      submitted[id] = { at: Date.now(), sourceId: payload.source_id, request: payload.request };
      // Only the run being watched is ever read back, so a short tail is enough
      // and the map cannot grow for the life of the page. Keys are insertion
      // ordered here because none of them looks like an array index.
      var held = Object.keys(submitted);
      if (held.length > SUBMIT_HISTORY) delete submitted[held[0]];
      return json({ id: id, status: 'pending' });
    }

    var match = path.match(/^\\/api\\/analyses\\/([0-9a-f]{16})$/);
    if (match && submitted[match[1]]) return json(run(match[1]));

    if (path.indexOf('/api/sources/') === 0 && path.indexOf('/daemon') !== -1) {
      var key = path.split('?')[0];
      return ROUTES[key] ? json(daemonView(key))
                         : Promise.resolve(new Response('{}', { status: 404 }));
    }

    if (ROUTES[path]) return json(shift(ROUTES[path]));
    // The launcher asks for exactly the routes the fixtures carry. Anything else
    // is a change upstream, and answering it with a plausible empty body would
    // hide that.
    if (path.indexOf('/api/') === 0) {
      return Promise.resolve(new Response('{}', { status: 404 }));
    }
    return realFetch(input, init);
  };

  // The report screen is an iframe, not a read, so fetch never sees it. The site
  // already hosts the engine's dashboard, so the demo closes on itself.
  new MutationObserver(function (records) {
    for (var i = 0; i < records.length; i++) {
      var added = records[i].addedNodes;
      for (var j = 0; j < added.length; j++) {
        var node = added[j];
        if (!node.querySelectorAll) continue;
        var frames = node.tagName === 'IFRAME' ? [node] : node.querySelectorAll('iframe');
        for (var k = 0; k < frames.length; k++) {
          var src = frames[k].getAttribute('src') || '';
          if (src.indexOf('/reports/') === 0) frames[k].setAttribute('src', '/exemple/dashboard');
        }
      }
    }
  }).observe(document.documentElement, { childList: true, subtree: true });

  // The launcher resolves its theme in a blocking script at the top of <head>,
  // so by the time this runs it has already read an empty store and fallen back
  // to prefers-color-scheme. Writing the store alone would apply on the next
  // load and leave a light launcher inside a dark page now, so the attribute is
  // set outright. This is where the launcher differs from the dashboard, whose
  // own resolution runs from an IIFE at the end of body.
  // The site owns the theme in a frame, and the launcher's own control reads its
  // label from application state this patch does not reach. Hidden rather than
  // fought, the same choice the dashboard embed makes.
  if (window.self !== window.top) {
    try {
      var hide = document.createElement('style');
      hide.textContent = '#theme-toggle{display:none!important}';
      (document.head || document.documentElement).appendChild(hide);
    } catch (e) {}
  }

  var initial = (location.search.match(/[?&]theme=(dark|light)(?:&|$)/) || [])[1];
  if (initial) {
    document.documentElement.setAttribute('data-theme', initial);
    document.documentElement.setAttribute('data-theme-position', initial);
    try {
      sessionStorage.setItem('perf-sentinel:theme', initial);
      localStorage.setItem('perf-sentinel:theme', initial);
    } catch (e) {}
  }

  // The launcher has no postMessage listener of its own, and neither store
  // crosses an origin, so the parent's theme arrives here and nowhere else.
  window.addEventListener('message', function (e) {
    if (e.origin !== location.origin) return;
    var t = e.data && e.data.psTheme;
    if (t !== 'light' && t !== 'dark') return;
    document.documentElement.setAttribute('data-theme', t);
    document.documentElement.setAttribute('data-theme-position', t);
    try { sessionStorage.setItem('perf-sentinel:theme', t); } catch (_) {}
  });
})();
</script>`;

rmSync(OUT_DIR, { recursive: true, force: true });
mkdirSync(OUT_DIR, { recursive: true });
cpSync(WWWROOT, OUT_DIR, { recursive: true });

const patched = `${source.slice(0, headEnd)}${PATCH}\n${source.slice(headEnd)}`;
writeFileSync(join(OUT_DIR, 'index.html'), patched);
console.log(`[embed-hub] wrote exemple/hub/ from ${WWWROOT}`);
console.log(`[embed-hub] ${Object.keys(fixtures.routes).length} routes replayed, patch injected before </head>`);
