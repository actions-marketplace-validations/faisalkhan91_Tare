// Fixture-backed browser E2E harness. Runs the SHELL journeys — route
// redirects, analysis-state persistence, keyboard command palette, theme, responsive layout, and the
// pre-data startup shell — against the BUILT assets (web/dist), served statically. The read API is
// stubbed per-test with fixtures (see e2e/fixtures.ts), so runs are deterministic and need no live
// backend. The required browser (chromium rev 1228, pinned by @playwright/test@1.61.1) is expected to
// be cached BEFORE CI mandates this suite — see e2e/README.md for the offline-cache contract.

import { defineConfig, devices } from "@playwright/test";

const PORT = 4599;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  timeout: 30_000,
  expect: { timeout: 5_000 },
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    // Useful failure artifacts without bloating green runs.
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    // Browser twin: all non-desktop specs load index.html (the HTTP-client build).
    { name: "chromium", testIgnore: /desktop\.spec\.ts/, use: { ...devices["Desktop Chrome"] } },
    // Desktop twin: desktop.spec.ts loads index.tauri.html behind a __TAURI__ stub (see tauriStub.ts).
    // macOS has no WKWebView WebDriver, so this is how the desktop chrome + seam get automated coverage.
    // 1100×720 matches the default window (gui.rs build_main_window); the spec resizes to the 420×520
    // minimum inline. colorScheme:dark exercises the dark chrome path deterministically.
    {
      name: "desktop-stub",
      testMatch: /desktop\.spec\.ts/,
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 1100, height: 720 },
        colorScheme: "dark",
        // Pin a macOS userAgent so applyOsClass resolves data-os="macos" (it only UA-sniffs off-native);
        // the WKWebView twin is macOS, so this is faithful, not a hack.
        userAgent:
          "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
      },
    },
  ],
  // Build the assets, then serve web/dist statically. Deterministic: every run exercises a fresh
  // build of the committed source. python3 is always present; no extra server dependency.
  // `npm run build` is the pure BROWSER build (what the committed embeds copy verbatim), so it does NOT
  // emit index.tauri.html; the desktop-stub project needs it served, so we copy it in here only — this
  // keeps the desktop-only entry out of web/dist and out of the browser/desktop embeds (build-ui.sh).
  webServer: {
    command: `npm run build && cp index.tauri.html dist/index.tauri.html && python3 -m http.server ${PORT} --directory dist`,
    url: `http://127.0.0.1:${PORT}/index.html`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
