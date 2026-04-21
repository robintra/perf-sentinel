import { test, expect, Page } from "@playwright/test";

const PATH = "/dashboard.html";

async function loadDashboard(page: Page, hash = "") {
  await page.goto(PATH + hash);
  await page.waitForSelector("[role=tablist]");
}

test("1. dashboard loads with filename in title", async ({ page }) => {
  await loadDashboard(page);
  await expect(page).toHaveTitle(/perf-sentinel: .+\.json/);
});

test("2. keyboard switches tabs via g-prefix shortcut", async ({ page }) => {
  await loadDashboard(page);
  await page.keyboard.press("g");
  await page.keyboard.press("p");
  await expect(page.locator("#tab-pgstat")).toHaveAttribute("aria-selected", "true");
});

test("3. clicking a finding row opens Explain", async ({ page }) => {
  await loadDashboard(page);
  const firstRow = page.locator("#findings-list .ps-row").first();
  await firstRow.click();
  await expect(page.locator("#tab-explain")).toHaveAttribute("aria-selected", "true");
  const breadcrumb = page.locator("#explain-breadcrumb");
  await expect(breadcrumb).toContainText("trace_id");
});

test("4. clicking a SQL span from Explain deep-links into pg_stat", async ({ page }) => {
  await loadDashboard(page);
  await page.locator("#findings-list .ps-row").first().click();
  const sqlLink = page.locator(".ps-span-pgstat-link").first();
  if (await sqlLink.count() === 0) {
    test.skip(true, "fixture has no SQL span with a matching pg_stat template");
  }
  await sqlLink.click();
  await expect(page.locator("#tab-pgstat")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#pgstat-drill")).toBeVisible();
});

test("5. search filter narrows pg_stat rows", async ({ page }) => {
  await loadDashboard(page, "#pgstat");
  const rowsBefore = await page.locator("#pgstat-body tr").count();
  expect(rowsBefore).toBeGreaterThan(0);
  await page.keyboard.press("/");
  await page.keyboard.type("order_item");
  const visibleAfter = await page
    .locator("#pgstat-body tr")
    .evaluateAll((nodes) => nodes.filter((n) => (n as HTMLElement).style.display !== "none").length);
  expect(visibleAfter).toBeLessThan(rowsBefore);
});

test("6. Export CSV blob carries RFC 4180-escaped content", async ({ page }) => {
  await loadDashboard(page);
  // Hook createObjectURL so the export click captures the blob text.
  await page.evaluate(() => {
    const g = globalThis as unknown as { __capturedCsv?: string };
    const originalCreate = URL.createObjectURL.bind(URL);
    URL.createObjectURL = (blob: Blob) => {
      blob.text().then((text) => { g.__capturedCsv = text; });
      return originalCreate(blob);
    };
  });
  await page.locator("#findings-export").click();
  const csv = await page.waitForFunction(() => (globalThis as unknown as { __capturedCsv?: string }).__capturedCsv);
  const body = await csv.jsonValue();
  const text = String(body);
  expect(text.length).toBeGreaterThan(0);

  const lines = text.split(/\r?\n/).filter((l) => l.length > 0);
  const header = lines[0];
  expect(header).toMatch(/^(type|severity|service|trace_id)/i);
  expect(header.split(",").length).toBeGreaterThan(3);
  expect(lines.length).toBeGreaterThan(1);

  function parseCsvRow(line: string): string[] {
    const cells: string[] = [];
    let cur = "";
    let i = 0;
    let inQuotes = false;
    while (i < line.length) {
      const c = line[i];
      if (inQuotes) {
        if (c === "\"" && line[i + 1] === "\"") {
          cur += "\"";
          i += 2;
        } else if (c === "\"") {
          inQuotes = false;
          i += 1;
        } else {
          cur += c;
          i += 1;
        }
      } else {
        if (c === "\"") {
          inQuotes = true;
          i += 1;
        } else if (c === ",") {
          cells.push(cur);
          cur = "";
          i += 1;
        } else {
          cur += c;
          i += 1;
        }
      }
    }
    cells.push(cur);
    return cells;
  }

  let foundQuotedComma = false;
  for (const line of lines.slice(1)) {
    const parsed = parseCsvRow(line);
    expect(parsed.length).toBe(header.split(",").length);
    for (const cell of parsed) {
      if (cell.includes(",")) {
        foundQuotedComma = true;
        const quoted = "\"" + cell.replace(/"/g, "\"\"") + "\"";
        expect(line).toContain(quoted);
      }
    }
  }
  expect(
    foundQuotedComma,
    "fixture must carry at least one template with a literal comma so RFC 4180 quoting is exercised"
  ).toBe(true);
});

test("7. hash deep-link applies state on fresh load", async ({ page }) => {
  await loadDashboard(page, "#pgstat&ranking=mean_time&search=order_item");
  await expect(page.locator("#tab-pgstat")).toHaveAttribute("aria-selected", "true");
  const chips = page.locator("#pgstat-rankings .ps-chip");
  await expect(chips.nth(2)).toHaveAttribute("aria-checked", "true");
  await expect(page.locator("#pgstat-search")).toHaveValue("order_item");
});

test("8. hashchange on in-page update applies state", async ({ page }) => {
  await loadDashboard(page, "#findings");
  await page.evaluate(() => {
    location.hash = "#pgstat&ranking=calls";
  });
  await page.waitForTimeout(50);
  await expect(page.locator("#tab-pgstat")).toHaveAttribute("aria-selected", "true");
  const chips = page.locator("#pgstat-rankings .ps-chip");
  await expect(chips.nth(1)).toHaveAttribute("aria-checked", "true");
});

test("9. Copy link button writes location.href to the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await loadDashboard(page, "#findings");
  await page.locator("#findings-copy-link").click();
  await page.waitForTimeout(50);
  const clip = await page.evaluate(() => navigator.clipboard.readText());
  expect(clip).toMatch(/#findings$/);
});

test("10. tablist carries ARIA roles and selection state", async ({ page }) => {
  await loadDashboard(page);
  const tablist = page.locator("[role=tablist]");
  await expect(tablist).toHaveCount(1);
  const tabs = page.locator("[role=tablist] [role=tab]");
  const count = await tabs.count();
  expect(count).toBeGreaterThan(0);
  const selectedCount = await page.locator("[role=tab][aria-selected=true]").count();
  expect(selectedCount).toBe(1);
  const falseCount = await page.locator("[role=tab][aria-selected=false]").count();
  expect(selectedCount + falseCount).toBe(count);
});
