import { execFileSync, spawn, ChildProcess } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
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
