import {ChildProcess, execFileSync, spawn} from "node:child_process";
import {existsSync, mkdirSync, readFileSync, writeFileSync} from "node:fs";
import {join, resolve} from "node:path";
import * as net from "node:net";

// Builds the release binary if missing, renders the HTML fixture,
// and spawns http-server on a free 127.0.0.1 port. HTTP origin is
// required by navigator.clipboard. See README.md for the rationale.

declare global {
  // eslint-disable-next-line no-var
  var __psServer: ChildProcess | undefined;
}

const REPO_ROOT = resolve(__dirname, "../../../..");
const BINARY = join(REPO_ROOT, "target/release/perf-sentinel");
const FIXTURE_JSON = join(REPO_ROOT, "tests/fixtures/report_realistic.json");
const PG_STAT_FIXTURE = join(REPO_ROOT, "tests/fixtures/pg_stat_statements.csv");
const FIXTURE_DIR = join(__dirname, "fixtures");
const MAIN_HTML = join(FIXTURE_DIR, "dashboard.html");
// Demo baseline (committed): a pre-analyzed Report JSON with the last
// finding (slow_http) dropped, so --before yields one regression on
// the Diff tab. The demo dashboard itself is rendered from the raw
// events fixture (so embedded_traces populate the Explain tab) and
// then patched in-place by `injectDemoCorrelations` to light up the
// Correlations tab. The batch pipeline never emits cross-trace
// correlations, so this patch is the only way to keep both the trace
// tree and the Correlations tab in one dashboard.
const DEMO_BASELINE = join(__dirname, "demo/fixtures/baseline.json");
const DEMO_HTML = join(FIXTURE_DIR, "dashboard-demo.html");
// Named `traces.json` so the banner + <title> in the demo dashboard
// match the command examples in the top-level README
// (`perf-sentinel report --input traces.json ...`).
const DEMO_EVENTS = join(FIXTURE_DIR, "traces.json");

// Inject a cloud_region on every event before the demo render so the
// GreenOps tab shows a multi-region breakdown with real operational
// CO2 numbers instead of a single "unknown" row at 0 gCO2. Pins one
// region per service, picking regions that ship hourly + seasonal
// grid intensity profiles (eu-west-3 = FR, us-east-1 = US-East,
// eu-central-1 = DE) so the scorer pulls a non-zero intensity. The
// shared test fixture (tests/fixtures/report_realistic.json) stays
// untouched, keeping the non-demo test suite unaffected.
const DEMO_REGION_BY_SERVICE: Record<string, string> = {
  "order-svc": "eu-west-3",
  "payment-svc": "us-east-1",
  "chat-svc": "eu-central-1"
};
const DEMO_REGION_FALLBACK = "eu-west-3";

const DEMO_CORRELATIONS = [
  {
    source: {
      finding_type: "n_plus_one_sql",
      service: "order-svc",
      template: "SELECT * FROM order_item WHERE order_id = ?"
    },
    target: {
      finding_type: "slow_http",
      service: "chat-svc",
      template: "POST /api/notify"
    },
    co_occurrence_count: 42,
    source_total_occurrences: 48,
    confidence: 0.875,
    median_lag_ms: 37.5,
    first_seen: "2026-04-20T09:55:12Z",
    last_seen: "2026-04-20T11:42:08Z",
    sample_trace_id: "trace-order-01"
  },
  {
    source: {
      finding_type: "redundant_sql",
      service: "payment-svc",
      template: "SELECT id, status FROM payment WHERE id = ?"
    },
    target: {
      finding_type: "n_plus_one_sql",
      service: "order-svc",
      template: "SELECT * FROM order_item WHERE order_id = ?"
    },
    co_occurrence_count: 19,
    source_total_occurrences: 22,
    confidence: 0.864,
    median_lag_ms: 12.1,
    first_seen: "2026-04-20T10:02:03Z",
    last_seen: "2026-04-20T11:38:44Z",
    sample_trace_id: "trace-payment-01"
  },
  {
    source: {
      finding_type: "slow_http",
      service: "chat-svc",
      template: "POST /api/notify"
    },
    target: {
      finding_type: "n_plus_one_sql",
      service: "order-svc",
      template: "SELECT * FROM order_item WHERE order_id = ?"
    },
    co_occurrence_count: 8,
    source_total_occurrences: 14,
    confidence: 0.571,
    median_lag_ms: 98.3,
    first_seen: "2026-04-20T10:15:00Z",
    last_seen: "2026-04-20T11:05:22Z"
  }
];

async function freePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const srv = net.createServer();
    srv.unref();
    srv.on("error", reject);
    srv.listen(0, "127.0.0.1", () => {
      const addr = srv.address();
      if (addr && typeof addr === "object") {
        const { port } = addr;
        srv.close(() => resolvePort(port));
      } else {
        reject(new Error("listen returned no address"));
      }
    });
  });
}

function buildBinaryIfMissing() {
  if (existsSync(BINARY)) return;
  // eslint-disable-next-line no-console
  console.log("[global-setup] building release binary (this can take a minute)");
  execFileSync(
    "cargo",
    ["build", "--release", "--bin", "perf-sentinel"],
    {
      cwd: REPO_ROOT,
      stdio: "inherit"
    }
  );
}

function renderFixtures() {
  if (!existsSync(FIXTURE_DIR)) {
    mkdirSync(FIXTURE_DIR, { recursive: true });
  }
  execFileSync(
    BINARY,
    [
      "report",
      "--input",
      FIXTURE_JSON,
      "--pg-stat",
      PG_STAT_FIXTURE,
      "--pg-stat-top",
      "15",
      "--output",
      MAIN_HTML
    ],
    { stdio: "inherit" }
  );
  // Demo dashboard: events enriched with per-service cloud_region
  // (so GreenOps renders a multi-region breakdown with real
  // operational CO2) + --before for the Diff tab. Correlations are
  // patched in post-render (see injectDemoCorrelations). Only used
  // by demo/tour.spec.ts; the regular test suite keeps hitting
  // dashboard.html built from the pristine fixture.
  writeDemoEvents(FIXTURE_JSON, DEMO_EVENTS);
  // `--daemon-url` flips the dashboard into live mode: per-finding
  // Ack/Revoke buttons, an Acknowledgments panel, a connection status
  // dot and a manual refresh button all become visible. The URL points
  // at a closed loopback port (65535) because the actual HTTP exchange
  // is intercepted by `injectDemoAckMock` below, no network call ever
  // leaves the page.
  execFileSync(
    BINARY,
    [
      "report",
      "--input",
      DEMO_EVENTS,
      "--before",
      DEMO_BASELINE,
      "--pg-stat",
      PG_STAT_FIXTURE,
      "--pg-stat-top",
      "15",
      "--daemon-url",
      "http://127.0.0.1:65535",
      "--output",
      DEMO_HTML
    ],
    { stdio: "inherit" }
  );
  injectDemoCorrelations(DEMO_HTML);
  injectDemoAckMock(DEMO_HTML);
}

// Copy the shared trace fixture and stamp a cloud_region on each
// event so the demo dashboard's GreenOps tab exercises the multi-
// region scorer. Events that already carry cloud_region are left
// alone so manual overrides survive future fixture additions.
function writeDemoEvents(source: string, dest: string) {
  const events: Array<Record<string, unknown>> = JSON.parse(readFileSync(source, "utf8"));
  for (const ev of events) {
    if (typeof ev.cloud_region === "string" && ev.cloud_region.length > 0) {
      continue;
    }
    const service = typeof ev.service === "string" ? ev.service : "";
    ev.cloud_region = DEMO_REGION_BY_SERVICE[service] ?? DEMO_REGION_FALLBACK;
  }
  writeFileSync(dest, JSON.stringify(events));
}

// Splice synthetic correlations + Electricity Maps scoring_config
// into the dashboard's embedded JSON payload. The batch pipeline
// emits neither (correlations need the daemon's rolling window,
// scoring_config needs a configured `[green.electricity_maps]`
// block with an auth token), so the demo patches them in
// post-render to surface the Correlations tab and the GreenOps
// scoring config bandeau in one dashboard. The script tag holds a
// JSON blob where every `</` is escaped to `<\/` (inject() in
// html.rs does this to block the script-tag-escape family of XSS
// defects), we unescape, mutate, and re-escape with the same rule.
//
// scoring_config is built with `direct` + `5_minutes` opt-ins
// (Scope 2 audit-grade profile) so the bandeau renders one v4
// neutral chip plus two accent chips, exercising every chip
// modifier in a single still.
function injectDemoCorrelations(htmlPath: string) {
  const START = '<script id="report-data" type="application/json">';
  const END = "</script>";
  const html = readFileSync(htmlPath, "utf8");
  const startIdx = html.indexOf(START);
  if (startIdx === -1) {
    throw new Error(`injectDemoCorrelations: ${START} not found in ${htmlPath}`);
  }
  const jsonStart = startIdx + START.length;
  const endIdx = html.indexOf(END, jsonStart);
  if (endIdx === -1) {
    throw new Error(`injectDemoCorrelations: ${END} not found after script tag`);
  }
  const escaped = html.slice(jsonStart, endIdx);
  const payload = JSON.parse(escaped.replace(/<\\\//g, "</"));
  payload.report = payload.report ?? {};
  payload.report.correlations = DEMO_CORRELATIONS;
  payload.report.green_summary = payload.report.green_summary ?? {};
  payload.report.green_summary.scoring_config = {
    api_version: "v4",
    emission_factor_type: "direct",
    temporal_granularity: "5_minutes"
  };
  const newEscaped = JSON.stringify(payload).replace(/<\//g, "<\\/");
  writeFileSync(htmlPath, html.slice(0, jsonStart) + newEscaped + html.slice(endIdx));
}

// Patch `window.fetch` inside the demo dashboard so the live-mode UI
// (Ack/Revoke buttons, Acknowledgments panel, connection status dot)
// renders without spawning a real daemon. The dashboard JS calls four
// endpoints when `--daemon-url` is set: GET `/api/status`, GET
// `/api/acks`, POST `/api/findings/{sig}/ack`, DELETE
// `/api/findings/{sig}/ack`. The mock answers each with the exact
// status code the dashboard expects (200 for status/acks, 201 for
// POST, 204 for DELETE) and keeps a small in-memory store so toggling
// "Show acknowledged" or revoking a row updates the panel as a real
// daemon would. Three pre-populated acks make the panel non-empty
// from the first paint, which is what the stills capture.
//
// The three signatures match real findings in report_realistic.json
// (idx 1, 3, 4 of the Findings list) so three of the five rows
// render with the "Revoke" button (acked state) instead of "Ack".
// This is what the "Show acknowledged" still captures, otherwise the
// toggle would have no visual effect on the demo dataset. Idx 0
// (POST /api/orders/7/checkout) is deliberately left un-acked so the
// existing `02 explain trace tree` still finds a visible first row to
// click without needing a `:visible` selector tweak.
function injectDemoAckMock(htmlPath: string) {
  // Anchor on the embedded report-data script tag (unique in the
  // template, see html_template.html line 407) and insert the mock
  // *before* it. The mock must be parsed and executed before any
  // dashboard bundle runs `pingStatus()` / `fetchAcks()` so the
  // patched `window.fetch` is in place for the very first call.
  const MARKER = '<script id="report-data"';
  const html = readFileSync(htmlPath, "utf8");
  const idx = html.indexOf(MARKER);
  if (idx === -1) {
    throw new Error(`injectDemoAckMock: ${MARKER} not found in ${htmlPath}`);
  }
  const insertAt = idx;
  const script = [
    "<script>(function () {",
    "  var originalFetch = window.fetch.bind(window);",
    "  var acks = [",
    "    {",
    '      signature: "n_plus_one_sql:order-svc:POST__api_orders_8_checkout:c69d2f3ae7f5c6cdb2c6762367852ec7",',
    '      by: "alice@example.com",',
    '      reason: "Known intentional batch in checkout, JIRA-1234",',
    '      at: "2026-05-02T09:14:22Z",',
    '      expires_at: "2026-05-09T09:14:22Z"',
    "    },",
    "    {",
    '      signature: "slow_http:chat-svc:POST__api_chat_send:5e35a0e005ee19f3a02169ed764b106d",',
    '      by: "bob@example.com",',
    '      reason: "Notify endpoint pending move to gRPC, see ADR-0042",',
    '      at: "2026-05-04T16:02:11Z",',
    "      expires_at: null",
    "    },",
    "    {",
    '      signature: "redundant_sql:payment-svc:GET__api_payment_999:48b668a2479a4b5b068b0cc99041abec",',
    '      by: "ops-bot",',
    '      reason: "Cache layer planned for 0.7.0 release",',
    '      at: "2026-05-06T11:48:00Z",',
    '      expires_at: "2026-06-30T00:00:00Z"',
    "    }",
    "  ];",
    "  function jsonResponse(status, body) {",
    "    return new Response(JSON.stringify(body), {",
    "      status: status,",
    '      headers: { "Content-Type": "application/json" }',
    "    });",
    "  }",
    "  function emptyResponse(status) {",
    "    return new Response(null, { status: status });",
    "  }",
    "  window.fetch = function (input, init) {",
    '    var url = typeof input === "string" ? input : (input && input.url) || "";',
    '    var method = (init && init.method) || (input && input.method) || "GET";',
    "    method = method.toUpperCase();",
    '    if (url.indexOf("/api/status") !== -1 && method === "GET") {',
    '      return Promise.resolve(jsonResponse(200, { ok: true, version: "demo-mock" }));',
    "    }",
    '    if (url.indexOf("/api/acks") !== -1 && method === "GET") {',
    "      return Promise.resolve(jsonResponse(200, acks));",
    "    }",
    "    var ackMatch = url.match(/\\/api\\/findings\\/([^/]+)\\/ack/);",
    "    if (ackMatch) {",
    "      var sig = decodeURIComponent(ackMatch[1]);",
    '      if (method === "POST") {',
    "        var body = {};",
    '        try { body = JSON.parse((init && init.body) || "{}"); } catch (_) {}',
    "        acks = acks.filter(function (a) { return a.signature !== sig; });",
    "        acks.push({",
    "          signature: sig,",
    '          by: body.by || "demo@perf-sentinel",',
    '          reason: body.reason || "(no reason)",',
    "          at: new Date().toISOString(),",
    "          expires_at: body.expires_at || null",
    "        });",
    "        return Promise.resolve(emptyResponse(201));",
    "      }",
    '      if (method === "DELETE") {',
    "        acks = acks.filter(function (a) { return a.signature !== sig; });",
    "        return Promise.resolve(emptyResponse(204));",
    "      }",
    "    }",
    "    return originalFetch(input, init);",
    "  };",
    "})();</script>",
    ""
  ].join("\n");
  writeFileSync(htmlPath, html.slice(0, insertAt) + script + html.slice(insertAt));
}

async function startStaticServer(): Promise<void> {
  const port = await freePort();
  const baseURL = `http://127.0.0.1:${port}`;
  process.env.PS_BASE_URL = baseURL;

  // `-a 127.0.0.1` is load-bearing. `http-server` defaults to binding
  // 0.0.0.0 otherwise, which would expose the fixture to every
  // interface on the runner for the duration of the suite.
  const server = spawn(
    "npx",
    [
      "--yes",
      "http-server",
      FIXTURE_DIR,
      "-p",
      String(port),
      "-a",
      "127.0.0.1",
      "-s",
      "--cors"
    ],
    { stdio: "pipe" }
  );
  globalThis.__psServer = server;

  server.on("error", (err) => {
    // eslint-disable-next-line no-console
    console.error("[global-setup] http-server failed:", err);
  });

  // Poll until the server answers, bail after 10s.
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(`${baseURL}/dashboard.html`, { method: "HEAD" });
      if (res.ok) return;
    } catch {
      // not yet ready
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`[global-setup] http-server did not respond at ${baseURL}`);
}

export default async function globalSetup() {
  if (!existsSync(FIXTURE_DIR)) mkdirSync(FIXTURE_DIR, { recursive: true });
  writeFileSync(join(FIXTURE_DIR, "index.html"), "<!-- perf-sentinel fixtures -->");

  buildBinaryIfMissing();
  renderFixtures();
  await startStaticServer();
}
