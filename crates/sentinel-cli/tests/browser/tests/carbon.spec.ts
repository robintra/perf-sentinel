import { test, expect, Page } from "@playwright/test";

// Carbon tab: the Energy card names where the energy figure comes from,
// never the window energy_model (an intensity tag on Electricity Maps
// daemons). The rules mirror GreenSummary::energy_source_label.

type Row = [service: string, tag: string, ratio: number];

async function loadCarbon(
  page: Page,
  energyKwh: number,
  windowTag: string,
  rows: Row[],
  calibrated = false,
) {
  await page.route("**/dashboard.html", async (route) => {
    const response = await route.fetch();
    const body = await response.text();
    const patched = body.replace(
      /(<script id="report-data" type="application\/json">\s*)([\s\S]*?)(\s*<\/script>)/,
      (_match, open, json, close) => {
        const payload = JSON.parse(json);
        const gs = payload.report.green_summary;
        gs.energy_kwh = energyKwh;
        gs.energy_model = windowTag;
        gs.energy_calibrated = calibrated;
        gs.per_service_energy_model = Object.fromEntries(rows.map(([s, t]) => [s, t]));
        gs.per_service_measured_ratio = Object.fromEntries(rows.map(([s, , r]) => [s, r]));
        return open + JSON.stringify(payload) + close;
      },
    );
    await route.fulfill({ response, body: patched });
  });
  await page.goto("/dashboard.html#green");
  await page.waitForSelector("#green-metrics .ps-metric");
}

function energyCard(page: Page) {
  return page.locator("#green-metrics > .ps-metric").nth(1);
}

async function expectEnergySub(page: Page, sub: string) {
  const card = energyCard(page);
  await expect(card.locator(".ps-metric-label")).toContainText("Energy");
  await expect(card.locator(".ps-metric-sub")).toHaveText(sub);
}

test("1. the Energy card sits right after Total CO2", async ({ page }) => {
  await loadCarbon(page, 0.0123, "io_proxy_v3", [["a", "io_proxy_v3", 0]]);
  const labels = page.locator("#green-metrics > .ps-metric .ps-metric-label");
  await expect(labels).toHaveCount(9);
  await expect(labels.nth(0)).toContainText("Total CO2");
  await expect(labels.nth(1)).toContainText("Energy");
  await expect(labels.nth(2)).toContainText("Operational CO2");
  await expect(energyCard(page).locator(".ps-metric-value")).toHaveText("0.012 kWh");
});

test("2. proxy-only energy reads as modeled from I/O counts", async ({ page }) => {
  await loadCarbon(page, 0.0123, "io_proxy_v3", [
    ["a", "io_proxy_v3", 0],
    ["b", "io_proxy_v3", 0],
  ]);
  await expectEnergySub(page, "modeled from I/O counts");
});

test("3. a mixed window names the backends and the covered share", async ({ page }) => {
  await loadCarbon(page, 0.5, "scaphandre_rapl", [
    ["a", "scaphandre_rapl", 1],
    ["b", "kepler_ebpf", 0.4],
    ["c", "io_proxy_v3", 0],
  ]);
  await expectEnergySub(
    page,
    "source kepler_ebpf, scaphandre_rapl on 2 of 3 services · rest modeled from I/O counts",
  );
});

test("4. full coverage names the backend once, without +cal", async ({ page }) => {
  await loadCarbon(page, 0.5, "scaphandre_rapl+cal", [
    ["a", "scaphandre_rapl+cal", 1],
    ["b", "scaphandre_rapl", 1],
  ]);
  await expectEnergySub(page, "source scaphandre_rapl");
});

test("5. a calibrated proxy says so", async ({ page }) => {
  await loadCarbon(page, 0.5, "io_proxy_v3+cal", [["a", "io_proxy_v3+cal", 0]]);
  await expectEnergySub(page, "modeled from I/O counts · calibrated");
});

test("5b. calibration behind a measured window tag still shows", async ({ page }) => {
  // The window tag drops +cal there, only energy_calibrated carries it.
  await loadCarbon(
    page,
    0.5,
    "scaphandre_rapl",
    [
      ["a", "scaphandre_rapl", 1],
      ["b", "scaphandre_rapl", 0],
    ],
    true,
  );
  await expectEnergySub(
    page,
    "source scaphandre_rapl on 1 of 2 services · rest modeled from I/O counts · calibrated",
  );
});

test("5c. calibration behind an Electricity Maps tag still shows", async ({ page }) => {
  await loadCarbon(page, 0.5, "electricity_maps_api", [["a", "electricity_maps_api", 0]], true);
  await expectEnergySub(page, "modeled from I/O counts · calibrated");
});

test("6. an Electricity Maps window tag is never shown as the energy source", async ({ page }) => {
  await loadCarbon(page, 0.5, "electricity_maps_api", [
    ["a", "electricity_maps_api", 0],
    ["b", "electricity_maps_api", 0],
  ]);
  await expectEnergySub(page, "modeled from I/O counts");
  await expect(page.locator("#green-metrics")).not.toContainText("electricity_maps_api");
});

test("7. no computed energy greys the card out", async ({ page }) => {
  await loadCarbon(page, 0, "", []);
  const card = energyCard(page);
  await expect(card.locator(".ps-metric-value")).toHaveText("-");
  await expect(card.locator(".ps-metric-value")).toHaveAttribute("data-tone", "muted");
  await expectEnergySub(page, "not computed · no span resolved to a region");
});

test("8. hostile tags are dropped, never rendered", async ({ page }) => {
  const long = "x".repeat(65);
  const bidi = "kepler\u202E_ebpf";
  await loadCarbon(page, 0.5, "io_proxy_v3", [
    ["a", long, 1],
    ["b", bidi, 1],
    ["c", "redfish_bmc", 1],
  ]);
  await expectEnergySub(page, "source redfish_bmc");
  const text = await page.locator("#green-metrics").innerText();
  expect(text).not.toContain(long);
  expect(text).not.toContain("\u202E");
});

test("9. at 900px the ninth card spans both columns", async ({ page }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await loadCarbon(page, 0.5, "io_proxy_v3", [["a", "io_proxy_v3", 0]]);
  const cards = page.locator("#green-metrics > .ps-metric");
  await expect(cards).toHaveCount(9);
  await expect(cards.nth(8).locator(".ps-metric-label")).toContainText("Broker waste");
  const first = await cards.nth(0).boundingBox();
  const second = await cards.nth(1).boundingBox();
  const last = await cards.nth(8).boundingBox();
  // Two columns: the second card starts the right column on row 1.
  expect(second!.y).toBeCloseTo(first!.y, 0);
  expect(second!.x).toBeGreaterThan(first!.x);
  // The last card runs from the left column to the right edge.
  expect(last!.x).toBeCloseTo(first!.x, 0);
  expect(last!.x + last!.width).toBeCloseTo(second!.x + second!.width, 0);
});
