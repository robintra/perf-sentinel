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

test("3. clicking a finding row opens the inline detail pane", async ({ page }) => {
  // The app-shell redesign folds Explain into the Findings master/detail
  // pane: clicking a row updates the detail in place, the Findings tab
  // stays active (there is no separate Explain tab).
  await loadDashboard(page, "#findings");
  const firstRow = page.locator("#findings-list .ps-row").first();
  await firstRow.click();
  await expect(page.locator("#tab-findings")).toHaveAttribute("aria-selected", "true");
  await expect(page.locator("#explain-content")).toBeVisible();
  const breadcrumb = page.locator("#explain-breadcrumb");
  await expect(breadcrumb).toContainText("trace_id");
});

test("4. clicking a SQL span from the detail tree deep-links into pg_stat", async ({ page }) => {
  await loadDashboard(page, "#findings");
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
  await loadDashboard(page, "#findings");
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
    expect(parsed).toHaveLength(header.split(",").length);
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
  // The top-bar box is the only search input and the sole source of truth.
  await expect(page.locator("#topbar-search")).toHaveValue("order_item");
});

test("8. hashchange on in-page update applies state", async ({ page }) => {
  await loadDashboard(page, "#findings");
  await page.evaluate(() => {
    location.hash = "#pgstat&ranking=calls";
  });
  // The assertion below auto-retries until the hashchange handler switches
  // the tab, so no fixed wait is needed.
  await expect(page.locator("#tab-pgstat")).toHaveAttribute("aria-selected", "true");
  const chips = page.locator("#pgstat-rankings .ps-chip");
  await expect(chips.nth(1)).toHaveAttribute("aria-checked", "true");
});

test("9. Copy link button writes location.href to the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await loadDashboard(page, "#findings");
  await page.locator("#findings-copy-link").click();
  // The clipboard write is async, so poll it instead of a fixed wait.
  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()))
    .toMatch(/#findings$/);
});

test("11. j key moves selection and the detail pane follows it", async ({ page }) => {
  // Master/detail: in the app-shell redesign the detail pane is always
  // visible beside the list, so j/k must re-render the detail, not just
  // move the highlight.
  await loadDashboard(page, "#findings");
  await page.waitForSelector("#findings-list .ps-row");
  const rowCount = await page.locator("#findings-list .ps-row").count();
  test.skip(rowCount < 2, "fixture needs at least two findings");
  await page.keyboard.press("j");
  const selectedType = await page
    .locator("#findings-list .ps-row.selected .ps-fin-type")
    .textContent();
  const detailType = await page.locator("#explain-detail-head .ps-detail-h2").textContent();
  expect(detailType?.trim()).toBe(selectedType?.trim());
});

test("12. density defaults to comfort and the toggle persists compact", async ({ page }) => {
  await loadDashboard(page);
  await expect(page.locator("html")).toHaveAttribute("data-density", "comfort");
  await page.locator("#density-toggle").click();
  await expect(page.locator("html")).toHaveAttribute("data-density", "compact");
  await page.reload();
  await page.waitForSelector("[role=tablist]");
  await expect(page.locator("html")).toHaveAttribute("data-density", "compact");
});

test("13. clicking a pg_stat header sorts the table and re-click reverses it", async ({ page }) => {
  await loadDashboard(page, "#pgstat");
  const callsHeader = page.locator("#pgstat-table thead th", { hasText: "Calls" });
  const callsColumn = async () =>
    (await page.locator("#pgstat-body tr td:nth-child(2)").allTextContents())
      .map((s) => parseFloat(s.replace(/[^0-9.+-]/g, "")));
  const defaultOrder = await callsColumn();
  expect(defaultOrder.length).toBeGreaterThan(1);
  // The rows must actually move, not just the header attribute: assert the
  // full column ordering, or a dead applyTableSort would still pass.
  await callsHeader.click();
  await expect(callsHeader).toHaveAttribute("aria-sort", "descending");
  const desc = await callsColumn();
  expect(desc).toEqual([...desc].sort((a, b) => b - a));

  await callsHeader.click();
  await expect(callsHeader).toHaveAttribute("aria-sort", "ascending");
  const asc = await callsColumn();
  expect(asc).toEqual([...asc].sort((a, b) => a - b));
  expect(asc).toEqual([...desc].reverse());

  // Third click restores the report's own order.
  await callsHeader.click();
  await expect(callsHeader).not.toHaveAttribute("aria-sort", /.*/);
  expect(await callsColumn()).toEqual(defaultOrder);
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

test("23. Diff and Correlations explain themselves when a search empties them", async ({ page }) => {
  // The query is global, so it can blank a panel the user never typed on.
  // A blank list under a header claiming N items reads as a broken report.
  await page.goto("/dashboard-demo.html#diff");
  await page.waitForSelector("[role=tablist]");
  test.skip(await page.locator("#tab-diff").count() === 0, "demo fixture has no diff");

  await page.locator("#topbar-search").fill("zzz-matches-nothing");
  for (const id of ["diff-new", "diff-resolved"]) {
    await expect(page.locator(`#${id}-header`), "the header count must follow the search")
      .toContainText("(0)");
    await expect(page.locator(`#${id}-empty`)).toBeVisible();
    await expect(page.locator(`#${id}-empty`)).toContainText("match the current search");
  }

  await page.locator("#tab-correlations").click();
  await expect(page.locator("#correlations-empty")).toBeVisible();
  await expect(page.locator("#correlations-empty")).toContainText("match the current search");

  // Clearing the query restores the rows and hides the empty states again.
  // "/" focuses the box first, which is what Escape's "close search" tier
  // acts on.
  await page.keyboard.press("/");
  await page.keyboard.press("Escape");
  await expect(page.locator("#correlations-empty")).toBeHidden();
  expect(await page.locator("#correlations-list .ps-corr-card").count()).toBeGreaterThan(0);
});

test("19. searching a finding by the type label shown on its row finds it", async ({ page }) => {
  // The findings search matches the data, not the row text, so the blob has to
  // carry the display label ("N+1 SQL") and not just the raw slug
  // ("n_plus_one_sql"), or the query a user can actually read returns nothing.
  await loadDashboard(page, "#findings");
  const label = (await page.locator("#findings-list .ps-fin-type").first().textContent())!.trim();
  expect(label.length).toBeGreaterThan(0);

  await page.locator("#topbar-search").fill(label);
  expect(await page.locator("#findings-list .ps-row").count(),
    `searching the displayed label ${JSON.stringify(label)} must match its own row`)
    .toBeGreaterThan(0);

  // The raw slug still matches, so deep links and scripted queries keep working.
  await page.locator("#topbar-search").fill("n_plus_one");
  expect(await page.locator("#findings-list .ps-row").count()).toBeGreaterThan(0);
});

test("20. a deep link carrying a search leaves findings consistent on any tab", async ({ page }) => {
  // The query is global, so landing on pg_stat still has to rebuild the
  // findings list: a stale list under a filtered badge contradicts itself.
  await loadDashboard(page, "#pgstat&search=order_item");
  const badge = Number(await page.locator("#tab-findings .ps-nav-badge").textContent());
  await page.locator("#tab-findings").click();
  const rows = await page.locator("#findings-list .ps-row").count();
  expect(rows, "the panel must agree with the badge the deep link produced").toBe(badge);
  const counter = await page.locator("#findings-count").textContent();
  expect(counter).toContain(`${badge} findings`);
});

test("21. a slash typed into a text field is not stolen by the search shortcut", async ({ page }) => {
  // "/" focuses the search box, but only when no text field owns the caret.
  // The ack reason is a free-text field and slashes are common in ticket refs.
  await page.goto("/dashboard-demo.html#findings");
  await page.waitForSelector("[role=tablist]");
  const ackBtn = page.locator("#findings-list .ps-fin-action-btn").first();
  test.skip(await ackBtn.count() === 0, "demo fixture is not in live mode");

  await ackBtn.click();
  await page.locator("#ack-modal-reason").click();
  await page.keyboard.type("see ops/1234");
  await expect(page.locator("#ack-modal-reason")).toHaveValue("see ops/1234");
  expect(await page.evaluate(() => document.activeElement!.id)).toBe("ack-modal-reason");
});

test("22. the Ack button still works after the search re-renders the rows", async ({ page }) => {
  // Typing rebuilds the findings rows, so the per-row Ack listener has to be
  // re-attached. Otherwise live mode locks the user out of acking.
  await page.goto("/dashboard-demo.html#findings");
  await page.waitForSelector("[role=tablist]");
  test.skip(await page.locator("#findings-list .ps-fin-action-btn").count() === 0,
    "demo fixture is not in live mode");

  await page.locator("#topbar-search").fill("s");
  await page.locator("#findings-list .ps-fin-action-btn").first().click();
  await expect(page.locator("#ack-modal")).toBeVisible();
});

test("18. the findings search counts and reaches past the rendered page", async ({ page }) => {
  // dashboard-many.html carries more findings than LIST_CAP (8) renders at
  // once. The search filters the findings data rather than the rendered rows,
  // so the badge must report every match and Show more must reveal matches
  // from later pages. Counting the DOM instead would cap the badge at 8 and
  // search only the first page.
  await page.goto("/dashboard-many.html#findings");
  await page.waitForSelector("[role=tablist]");
  const badge = page.locator("#tab-findings .ps-nav-badge");
  const domRows = () => page.locator("#findings-list .ps-row").count();

  const total = Number(await badge.textContent());
  expect(total, "fixture must exceed LIST_CAP for this test to mean anything")
    .toBeGreaterThan(8);
  expect(await domRows()).toBe(8);

  // Every finding carries "svc" in its service name, so all of them match.
  await page.locator("#topbar-search").fill("svc");
  expect(Number(await badge.textContent()), "badge must not cap at the rendered page")
    .toBe(total);
  expect(await domRows(), "the list stays paginated").toBe(8);

  // Reveal a later page: its rows must be search matches too.
  await page.locator("#findings-show-more").click();
  expect(await domRows()).toBeGreaterThan(8);
  const allMatch = await page.locator("#findings-list .ps-row").evaluateAll(
    (nodes) => nodes.every((n) => n.textContent!.toLowerCase().includes("svc"))
  );
  expect(allMatch, "rows revealed past page 1 must still match the query").toBe(true);

  // A query that matches nothing empties the list and zeroes the badge.
  await page.locator("#topbar-search").fill("zzz-matches-nothing");
  await expect(badge).toHaveText("0");
  expect(await domRows()).toBe(0);
});

test("14. the top-bar box is the only search input on every tab", async ({ page }) => {
  // Guards against reintroducing a per-panel search input beside the
  // top-bar one, which is how the two-visible-bars duplicate first crept in.
  await loadDashboard(page);
  for (const tab of ["findings", "pgstat", "mysqlstat", "diff", "correlations"]) {
    // The panels are static template markup, but a tab is only registered
    // when the payload carries its data, so gate on the nav button.
    if (await page.locator(`#tab-${tab}`).count() === 0) continue;
    await page.locator(`#tab-${tab}`).click();
    await expect(page.locator(`#panel-${tab} input[type=search]`)).toHaveCount(0);
  }
  const visibleSearches = page.locator("input[type=search]:visible");
  await expect(visibleSearches).toHaveCount(1);
  await expect(visibleSearches).toHaveAttribute("id", "topbar-search");
});

test("15. search from a non-searchable tab reports matches in the nav badges", async ({ page }) => {
  // Typing from Overview used to be swallowed silently. The badge is the
  // feedback surface now, so the count must react without leaving the tab.
  await loadDashboard(page, "#overview");
  const badge = page.locator("#tab-findings .ps-nav-badge");
  const total = Number(await badge.textContent());
  expect(total).toBeGreaterThan(0);

  await page.locator("#topbar-search").fill("zzz-matches-nothing");
  await expect(badge).toHaveText("0");

  // Escape clears the query and restores the registered total.
  await page.keyboard.press("Escape");
  await expect(badge).toHaveText(String(total));
});

test("16. the search survives a tab switch and marks the revealed panel", async ({ page }) => {
  await loadDashboard(page, "#pgstat");
  await page.locator("#topbar-search").fill("order_item");
  const visibleOnPgstat = await page
    .locator("#pgstat-body tr")
    .evaluateAll((nodes) => nodes.filter((n) => (n as HTMLElement).style.display !== "none").length);
  expect(visibleOnPgstat).toBeGreaterThan(0);

  // Leave and come back: the query used to be wiped on every switch.
  await page.locator("#tab-findings").click();
  await page.locator("#tab-pgstat").click();
  await expect(page.locator("#topbar-search")).toHaveValue("order_item");
  const visibleAfter = await page
    .locator("#pgstat-body tr")
    .evaluateAll((nodes) => nodes.filter((n) => (n as HTMLElement).style.display !== "none").length);
  expect(visibleAfter).toBe(visibleOnPgstat);
  // Marks are applied lazily, only to the panel on screen.
  expect(await page.locator("#pgstat-body mark.ps-mark").count()).toBeGreaterThan(0);
});

test("17. pg_stat actions stay grouped and right-aligned at every width", async ({ page }) => {
  // Layout guard without screenshots. The auto margin that pushes the actions
  // right moved off the deleted filter box onto a wrapper around both
  // buttons. It must sit on the wrapper, not on one button: the controls row
  // wraps below 920px, and a margin on the export button alone left the copy
  // link stranded on the next line, left-aligned. The narrow viewport is the
  // point of this test, a default-width check passes either way.
  for (const width of [1440, 1024, 780]) {
    await page.setViewportSize({ width, height: 900 });
    await loadDashboard(page, "#pgstat");
    // Measure the buttons themselves rather than their wrapper, so this
    // asserts the visible outcome and not the mechanism that achieves it.
    const controls = await page.locator("#panel-pgstat .ps-pgstat-controls").boundingBox();
    const exportBox = await page.locator("#pgstat-export").boundingBox();
    const copyBox = await page.locator("#pgstat-copy-link").boundingBox();
    expect(controls, `controls row missing at ${width}px`).not.toBeNull();

    expect(Math.abs(exportBox!.y - copyBox!.y), `buttons split across lines at ${width}px`)
      .toBeLessThan(2);
    const rightmost = Math.max(exportBox!.x + exportBox!.width, copyBox!.x + copyBox!.width);
    const gap = (controls!.x + controls!.width) - rightmost;
    expect(Math.abs(gap), `buttons not right-aligned at ${width}px`).toBeLessThan(2);
  }
});

test("24. a correlation opens the target-side detail with its span highlighted", async ({ page }) => {
  // sample_trace_id is the target-side finding's trace, so the synthetic
  // finding must describe the target (type + severity, never "undefined"), and
  // its template must be the one in that trace so the tree highlights the span.
  await page.goto("/dashboard-demo.html#correlations");
  await page.waitForSelector("[role=tablist]");
  const card = page.locator(".ps-correlation-clickable").first();
  test.skip(await card.count() === 0, "demo fixture has no clickable correlation");
  await card.click();
  const h2 = page.locator("#explain-detail-head .ps-detail-h2");
  await expect(h2).toBeVisible();
  await expect(h2).not.toHaveText(/undefined/i);
  await expect(h2).not.toHaveText("");
  // The detail describes the target-side finding whose trace this is, so its
  // offending span is present and highlighted in the tree.
  expect(await page.locator("#explain-tree .hilite").count()).toBeGreaterThan(0);
});

test("25. a hash naming an absent service does not silently empty the list", async ({ page }) => {
  // severity is validated against the three known values, and service must be
  // validated the same way, or a stale/hand-edited hash filters to nothing with no
  // active chip to explain the empty list.
  await loadDashboard(page, "#findings&service=ghost-svc-does-not-exist");
  expect(await page.locator("#findings-list .ps-row").count(),
    "an unknown service must be ignored, not applied").toBeGreaterThan(0);
  const active = await page.locator("#findings-filters .ps-chip.active").getAttribute("data-key");
  expect(active).toBe("all");

  // A real service from the data still applies.
  await loadDashboard(page, "#findings&service=order-svc");
  expect(await page.locator("#findings-filters .ps-chip.active").getAttribute("data-key"))
    .toBe("svc:order-svc");
});

test("26. wide tables scroll inside their card instead of being clipped", async ({ page }) => {
  // .ps-card clips with overflow:hidden. A table wider than the card (the acks
  // signatures at a narrow width) must scroll in place, and the page body must
  // never scroll horizontally.
  await page.setViewportSize({ width: 760, height: 900 });
  await page.goto("/dashboard-demo.html#acknowledgments");
  await page.waitForSelector("[role=tablist]");
  test.skip(await page.locator("#acks-table").count() === 0, "demo fixture is not in live mode");

  const state = await page.evaluate(() => {
    const card = document.getElementById("acks-table")!.closest(".ps-card") as HTMLElement;
    const de = document.documentElement;
    return {
      overflowX: getComputedStyle(card).overflowX,
      scrollable: card.scrollWidth > card.clientWidth,
      // documentElement, not body: body.clientWidth is skewed by the vertical
      // scrollbar and gives a false positive.
      docHOverflow: de.scrollWidth - de.clientWidth,
    };
  });
  expect(state.overflowX).toBe("auto");
  expect(state.scrollable, "the card must actually be scrollable when the table overflows").toBe(true);
  expect(state.docHOverflow, "the page must never scroll horizontally").toBeLessThanOrEqual(1);
});

test("27. the live-mode topbar wraps instead of overflowing on a narrow viewport", async ({ page }) => {
  // Live mode packs the most into the topbar (status, Refresh, density, theme).
  // Below ~760px those must wrap to a second line, not push the page into a
  // horizontal scroll.
  await page.setViewportSize({ width: 720, height: 900 });
  await page.goto("/dashboard-demo.html#overview");
  await page.waitForSelector("[role=tablist]");
  test.skip(await page.locator("#ps-daemon-status").isVisible() === false, "not live mode");
  const overflow = await page.evaluate(() =>
    document.documentElement.scrollWidth - document.documentElement.clientWidth);
  expect(overflow, "topbar must wrap, the page must not scroll horizontally").toBeLessThanOrEqual(1);
});
