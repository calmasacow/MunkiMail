import { defineConfig } from "@playwright/test";
export default defineConfig({
  testDir: "./src",
  testMatch: "smoke.spec.mjs",
  fullyParallel: false,
  workers: 1,
  timeout: 30000,
  outputDir: "test-results",
  reporter: "list",
  use: {
    baseURL: "http://127.0.0.1:1421",
    viewport: { width: 1440, height: 1000 },
    headless: true,
    launchOptions: {
      ...(process.env.CHROMIUM_PATH
        ? { executablePath: process.env.CHROMIUM_PATH }
        : {}),
      args: ["--no-sandbox"],
    },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npm run dev -- --port 1421",
    url: "http://127.0.0.1:1421",
    reuseExistingServer: !process.env.CI,
    timeout: 30000,
  },
});
