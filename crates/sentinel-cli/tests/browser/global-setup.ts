import { execFileSync, spawn, ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import * as net from "node:net";

// Global setup runs once before the Playwright suite. Responsibilities:
//   1. Build the release binary if missing. Skipped when a prebuilt
//      binary exists (CI builds it in a prior step; local runs use
//      whatever is at target/release/perf-sentinel).
//   2. Render two HTML fixtures by shelling out to that binary.
//   3. Spawn http-server rooted at the fixture directory on a free
//      port, expose the base URL via process.env.PS_BASE_URL.
//
// Teardown in global-teardown.ts stops the server.

declare global {
  // Populated by setup, consumed by teardown.
  // eslint-disable-next-line no-var
  var __psServer: ChildProcess | undefined;
}

const REPO_ROOT = resolve(__dirname, "../../../..");
const BINARY = join(REPO_ROOT, "target/release/perf-sentinel");
const FIXTURE_JSON = join(REPO_ROOT, "tests/fixtures/report_realistic.json");
const PG_STAT_FIXTURE = join(REPO_ROOT, "tests/fixtures/pg_stat_statements.csv");
const FIXTURE_DIR = join(__dirname, "fixtures");
const MAIN_HTML = join(FIXTURE_DIR, "dashboard.html");

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
  // Produce a dashboard with findings + pg_stat populated. Diff is
  // not needed for the current spec set (the resolved-row click
  // behavior stays covered by the Rust-side tests) and `--before`
  // would need a pre-rendered baseline Report JSON that this
  // fixture tree does not carry. Keep the setup minimal.
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
}

async function startStaticServer(): Promise<void> {
  const port = await freePort();
  const baseURL = `http://127.0.0.1:${port}`;
  process.env.PS_BASE_URL = baseURL;

  const server = spawn(
    "npx",
    ["--yes", "http-server", FIXTURE_DIR, "-p", String(port), "-s", "--cors"],
    { stdio: "pipe" }
  );
  globalThis.__psServer = server;

  server.on("error", (err) => {
    // eslint-disable-next-line no-console
    console.error("[global-setup] http-server failed:", err);
  });

  // Wait for the server to accept a connection. Polling with a low
  // interval keeps startup fast (typical <500ms) without a fixed
  // sleep. Bail after 10s to surface a real failure instead of
  // hanging the suite.
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
  // Write a tiny stub file in the fixture dir so the server has
  // something to serve from the moment it's ready, and so teardown
  // can detect setup-failed states safely.
  if (!existsSync(FIXTURE_DIR)) mkdirSync(FIXTURE_DIR, { recursive: true });
  writeFileSync(join(FIXTURE_DIR, "index.html"), "<!-- perf-sentinel fixtures -->");

  buildBinaryIfMissing();
  renderFixtures();
  await startStaticServer();
}
