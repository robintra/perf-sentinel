import { test } from "@playwright/test";
import { resolve } from "node:path";

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

test("01 findings with severity + service filters", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await page.locator('#findings-filters .ps-chip[data-key="sev:warning"]').click();
  await page.locator('#findings-filters .ps-chip[data-key="svc:order-svc"]').click();
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("findings", theme) });
});

test("02 explain trace tree", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await page.locator("#findings-list .ps-row").first().click();
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("explain", theme) });
});

test("03 pg_stat Calls ranking", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#pgstat&ranking=calls");
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("pg-stat", theme) });
});

test("04 diff regression", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#diff");
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("diff", theme) });
});

test("05 correlations cross-trace", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#correlations");
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("correlations", theme) });
});

test("06 greenops regions breakdown", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme, "#green");
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("greenops", theme) });
});

test("07 cheatsheet modal", async ({ page }, info) => {
  const theme = themeFor(info.project.name);
  await openDashboard(page, theme);
  await page.keyboard.press("?");
  await page.waitForSelector("#cheatsheet[open]");
  await page.waitForTimeout(150);
  await page.screenshot({ path: outPath("cheatsheet", theme) });
});
