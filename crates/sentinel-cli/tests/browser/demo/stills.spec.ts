import {expect, test} from "@playwright/test";
import {resolve} from "node:path";
import {statSync} from "node:fs";

// Verify the captured PNG was actually written and has plausible
// content. 1 KiB floor catches a write that produced a header-only
// file (corrupt or zero-byte) without coupling to pixel-level content.
function expectScreenshotWritten(path: string) {
  expect(statSync(path).size).toBeGreaterThan(1024);
}

// Captures one clean screenshot per tab for the docs. Runs twice
// (once per dashboard-stills-* project) to produce light + dark
// pairs that the README's <picture> tags can serve based on
// prefers-color-scheme. Light is the un-suffixed name (slot of
// <img src=...>); dark gets the -dark suffix (slot of
// <source srcset=...>).

const REPO_ROOT = resolve(__dirname, "../../../../..");
const OUT_DIR = resolve(REPO_ROOT, "docs/img/report");
const PATH = "/dashboard-demo.html";

function themeFor(projectName: string): "dark" | "light" {
  if (projectName.endsWith("-dark")) return "dark";
  if (projectName.endsWith("-light")) return "light";
  // Throw rather than silently default so a misnamed or newly added
  // project surfaces here instead of producing a still in the wrong
  // theme that quietly overwrites a committed asset.
  throw new Error(
    `themeFor: project name "${projectName}" must end with -dark or -light`
  );
}

function outPath(name: string, theme: "dark" | "light"): string {
  const file = theme === "dark" ? `${name}-dark.png` : `${name}.png`;
  return resolve(OUT_DIR, file);
}

async function openDashboard(
  page: import("@playwright/test").Page,
  theme: "dark" | "light",
  hash = ""
) {
  await page.addInitScript((t) => {
    try { sessionStorage.setItem("perf-sentinel:theme", t); } catch {}
  }, theme);
  await page.goto(PATH + hash);
  await page.waitForSelector("[role=tablist]");
  // Small settle so chip transitions and tab highlights stabilise.
  await page.waitForTimeout(200);
}

// Capture only the viewport (no fullPage). Modal stills (cheatsheet,
// ack-modal) need this because the HTML5 `<dialog>` element is
// positioned relative to the viewport, not the document. A fullPage
// screenshot would render the modal high in the frame whenever the
// underlying page extends past the 900 px viewport (which it does in
// live mode, with the Acks tab adding rows). Viewport-only keeps the
// dialog vertically centred and the dimmed backdrop covering the
// whole capture.
async function viewportScreenshot(
  page: import("@playwright/test").Page,
  path: string
) {
  await page.screenshot({ path, fullPage: false });
}

// Capture full viewport width but clip vertically to the bottom of the
// `.ps-footer` element (last visible content) plus a small padding.
// Avoids both the empty space below short tabs (diff, greenops) and the
// truncation of long tabs (pg_stat) when the viewport is fixed.
async function clipScreenshot(
  page: import("@playwright/test").Page,
  path: string
) {
  const height = await page.evaluate(() => {
    // `.ps-credit` is the very last visible element ("Powered by ..."),
    // sitting just below `.ps-footer` (the keyboard shortcuts line).
    const credit = document.querySelector(".ps-credit");
    if (credit) {
      const rect = credit.getBoundingClientRect();
      return Math.ceil(rect.bottom + window.scrollY + 16);
    }
    return Math.max(
      document.body.scrollHeight,
      document.documentElement.scrollHeight
    );
  });
  const viewport = page.viewportSize();
  const width = viewport?.width ?? 1280;
  await page.screenshot({
    path,
    fullPage: true,
    clip: { x: 0, y: 0, width, height: Math.max(height, 200) },
  });
}

test("01 findings with severity + service filters", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await page.locator('#findings-filters .ps-chip[data-key="sev:warning"]').click();
  await page.locator('#findings-filters .ps-chip[data-key="svc:order-svc"]').click();
  await page.waitForTimeout(150);
  const path = outPath("findings", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("02 explain trace tree", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await page.locator("#findings-list .ps-row").first().click();
  await page.waitForTimeout(150);
  const path = outPath("explain", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("03 pg_stat Calls ranking", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#pgstat&ranking=calls");
  await page.waitForTimeout(150);
  const path = outPath("pg-stat", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("04 diff regression", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#diff");
  await page.waitForTimeout(150);
  const path = outPath("diff", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("05 correlations cross-trace", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#correlations");
  await page.waitForTimeout(150);
  const path = outPath("correlations", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("06 greenops regions breakdown", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#green");
  await page.waitForTimeout(150);
  const path = outPath("greenops", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("07 cheatsheet modal", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await page.keyboard.press("?");
  await page.waitForSelector("#cheatsheet[open]");
  await page.waitForTimeout(150);
  const path = outPath("cheatsheet", theme);
  await viewportScreenshot(page, path);
  expectScreenshotWritten(path);
});

// Wait until the dashboard has finished its live-mode boot: status
// ping returned 200 (dot turns green) AND the acks fetch landed (the
// Acks tab badge shows the mocked count). Avoids the timing-dependent
// `waitForTimeout` that flaked on slower runners when the chain
// status -> acks -> render exceeded the fixed sleep.
async function waitForLiveAcks(
  page: import("@playwright/test").Page,
  expectedAcks: number
) {
  await page.waitForSelector("#ps-daemon-dot.connected");
  await page.waitForFunction(
    (expected) => {
      const badge = document.querySelector(
        "#tab-acknowledgments .ps-badge"
      );
      return badge !== null && badge.textContent?.trim() === String(expected);
    },
    expectedAcks
  );
}

test("08 ack modal open", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await waitForLiveAcks(page, 3);
  // Three of the five visible findings are pre-acked by the mock, so
  // those rows render with "Revoke" and clicking them would prompt
  // for revoke instead of opening the ack modal. The selector below
  // walks past them to the first row still in "Ack" state.
  const ackBtn = page
    .locator("#findings-list .ps-row .ps-fin-action-btn:not(.revoke)")
    .first();
  await ackBtn.click();
  await page.waitForSelector("#ack-modal[open]");
  await page.waitForTimeout(150);
  const path = outPath("ack-modal", theme);
  await viewportScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("09 acknowledgments panel", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#acknowledgments");
  await waitForLiveAcks(page, 3);
  await page.waitForFunction(
    () => document.querySelectorAll("#acks-body tr").length === 3
  );
  const path = outPath("ack-panel", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});

test("10 show acknowledged toggle", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await waitForLiveAcks(page, 3);
  await page.locator("#findings-include-acked").check();
  await page.waitForTimeout(150);
  const path = outPath("ack-toggle", theme);
  await clipScreenshot(page, path);
  expectScreenshotWritten(path);
});
