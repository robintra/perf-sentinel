import {defineConfig, devices} from "@playwright/test";

// Demo-only Playwright config. Drives four artefact groups that
// `npm run demo` ships to docs/img/report/:
//   - dashboard_dark.gif  (dashboard-dark project, tour.spec.ts)
//   - dashboard_light.gif (dashboard-light project, tour.spec.ts)
//   - findings-dark.png, ..., cheatsheet-dark.png
//     (dashboard-stills-dark project, stills.spec.ts)
//   - findings.png, ..., cheatsheet.png (light baseline names to
//     match the <img src=...> slot of a <picture> element)
//     (dashboard-stills-light project, stills.spec.ts)
//
// Kept separate from playwright.config.ts so `npx playwright test`
// (and CI) never picks up the demo as a regular test.

// 1280x900 keeps long content (15+ pg_stat rows + footer) fully in
// frame for the tour video. Stills clip to content height in
// stills.spec.ts, so short tabs (diff, greenops) stay compact even
// with this taller viewport.
const VIEWPORT = { width: 1280, height: 900 } as const;

export default defineConfig({
  testDir: "./demo",
  fullyParallel: false,
  forbidOnly: false,
  retries: 0,
  workers: 1,
  outputDir: "./demo-videos",
  reporter: [["list"]],
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  use: {
    baseURL: process.env.PS_BASE_URL || "http://127.0.0.1:4123",
    trace: "off",
    screenshot: "off",
    viewport: VIEWPORT
  },
  projects: [
    {
      name: "dashboard-dark",
      testMatch: "tour.spec.ts",
      use: {
        ...devices["Desktop Chrome"],
        viewport: VIEWPORT,
        colorScheme: "dark",
        video: { mode: "on", size: VIEWPORT },
        permissions: ["clipboard-read", "clipboard-write"]
      }
    },
    {
      name: "dashboard-light",
      testMatch: "tour.spec.ts",
      use: {
        ...devices["Desktop Chrome"],
        viewport: VIEWPORT,
        colorScheme: "light",
        video: { mode: "on", size: VIEWPORT },
        permissions: ["clipboard-read", "clipboard-write"]
      }
    },
    {
      name: "dashboard-stills-dark",
      testMatch: "stills.spec.ts",
      use: {
        ...devices["Desktop Chrome"],
        viewport: VIEWPORT,
        colorScheme: "dark",
        video: "off",
        permissions: ["clipboard-read", "clipboard-write"]
      }
    },
    {
      name: "dashboard-stills-light",
      testMatch: "stills.spec.ts",
      use: {
        ...devices["Desktop Chrome"],
        viewport: VIEWPORT,
        colorScheme: "light",
        video: "off",
        permissions: ["clipboard-read", "clipboard-write"]
      }
    }
  ]
});
