import { test } from "@playwright/test";

// Scripted tour of the HTML dashboard. Pacing is deliberate: each
// pause is tuned so the final GIF reads as a calm demo rather than a
// Benny Hill sketch. Adjust `pause` values if a step looks rushed or
// stalls when viewed at 15 fps.

async function pause(page: import("@playwright/test").Page, ms: number) {
  await page.waitForTimeout(ms);
}

test("dashboard tour", async ({ page }, testInfo) => {
  const primary = testInfo.project.name === "dashboard-dark" ? "dark" : "light";
  const opposite = primary === "dark" ? "light" : "dark";

  // Seed sessionStorage before navigation so the boot code renders
  // the intended primary theme on first paint (no auto-mode flash).
  await page.addInitScript((t) => {
    try { sessionStorage.setItem("perf-sentinel:theme", t); } catch {}
  }, primary);

  await page.goto("/dashboard-demo.html");
  await page.waitForSelector("[role=tablist]");
  await pause(page, 1500);

  // --- Findings exploration ---
  // Severity filter: Warning chip narrows the list to 3 warnings.
  await page.locator('#findings-filters .ps-chip[data-key="sev:warning"]').click();
  await pause(page, 1400);
  // Stack a service filter on top: order-svc leaves only the two N+1s.
  await page.locator('#findings-filters .ps-chip[data-key="svc:order-svc"]').click();
  await pause(page, 1400);
  // Reset with "All".
  await page.locator('#findings-filters .ps-chip[data-key="all"]').click();
  await pause(page, 900);

  // Click first finding row -> Explain opens with the trace tree.
  await page.locator("#findings-list .ps-row").first().click();
  await pause(page, 2400);

  // g p: jump to pg_stat via the vim-style shortcut.
  await page.keyboard.press("g");
  await pause(page, 200);
  await page.keyboard.press("p");
  await pause(page, 1500);

  // Switch ranking chip to "top by calls" (second chip).
  await page.locator("#pgstat-rankings .ps-chip").nth(1).click();
  await pause(page, 1500);

  // Open the search filter, type a few characters, clear with Esc.
  await page.keyboard.press("/");
  await pause(page, 300);
  await page.keyboard.type("order", { delay: 130 });
  await pause(page, 1400);
  await page.keyboard.press("Escape");
  await pause(page, 600);

  // Copy link: flashes "Copied" after writing location.href.
  await page.locator("#pgstat-copy-link").click();
  await pause(page, 1200);

  // g d: Diff tab. One new finding shows up as a regression.
  await page.keyboard.press("g");
  await pause(page, 200);
  await page.keyboard.press("d");
  await pause(page, 2600);

  // g c: Correlations tab. Three synthetic cross-trace pairs.
  await page.keyboard.press("g");
  await pause(page, 200);
  await page.keyboard.press("c");
  await pause(page, 2600);

  // --- Wink at the opposite theme and come back ---
  // Cycle auto -> dark -> light -> auto. One click advances one
  // notch; from a forced primary theme we need at most three clicks
  // to land on any of the three states. Short 300 ms pauses between
  // clicks keep the cycle readable without dragging the GIF. Throws
  // if the loop exits without reaching the target so a future cycle
  // change (e.g. adding a fourth state) surfaces loudly instead of
  // producing a silently-wrong GIF.
  const cycleTo = async (target: "dark" | "light") => {
    for (let i = 0; i < 3; i += 1) {
      const now = await page.evaluate(() =>
        document.documentElement.getAttribute("data-theme"));
      if (now === target) return;
      await page.locator("#theme-toggle").click();
      await pause(page, 300);
    }
    throw new Error(`cycleTo: theme cycle never reached "${target}"`);
  };
  await cycleTo(opposite);
  await pause(page, 1800);
  await cycleTo(primary);
  await pause(page, 500);

  // g r: jump to GreenOps, back in the primary theme.
  await page.keyboard.press("g");
  await pause(page, 200);
  await page.keyboard.press("r");
  await pause(page, 1800);

  // Cheatsheet modal via "?". Pressing `?` directly so the keydown
  // event carries key === "?", which is what the handler checks.
  await page.keyboard.press("?");
  await page.waitForSelector("#cheatsheet[open]", { timeout: 2000 });
  await pause(page, 2500);
  await page.keyboard.press("Escape");
  await pause(page, 600);
});
