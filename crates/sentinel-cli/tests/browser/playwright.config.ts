import { defineConfig, devices } from "@playwright/test";

// The dashboard is served over http:// by the static server spawned
// in global-setup.ts. See README.md for why file:// is not used.

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: [
    ["list"],
    ["html", { open: "never", outputFolder: "playwright-report" }]
  ],
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  use: {
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    baseURL: process.env.PS_BASE_URL || "http://127.0.0.1:4123"
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        permissions: ["clipboard-read", "clipboard-write"]
      }
    }
  ]
});
