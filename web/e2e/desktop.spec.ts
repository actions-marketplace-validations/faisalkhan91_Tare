// Desktop served-twin journey (tare desktop-testing layer). macOS has no WKWebView WebDriver, so this
// loads the SAME built assets the WebView loads — index.tauri.html + dist + desktop.css — in headless
// Chromium behind a `window.__TAURI__` stub, and proves the desktop-only wiring the 11 browser specs
// (which load index.html) never touch: the invoke-transport boot, desktop.css load-order, the
// traffic-light seam as engine-independent geometry, and every native-event bridge in bootTauri.ts.
//
// What this layer CANNOT see (native compositor only — the manual checklist in docs/DESKTOP_TESTING.md):
// real vibrancy, WKWebView font rasterization, the native traffic-light rendering, window-drag hit
// testing, the measured-reserve init-script branch (the twin always takes the 96px default), and the
// real invoke marshaling (the stub short-circuits it). This is the fast majority-coverage layer, not a
// substitute for the real-WKWebView pass.

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";
import { installTauriStub } from "./tauriStub.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
  await installTauriStub(page); // defaults to macOS + vibrancy + the 96px reserve fallback
});

/// The LIVE traffic-light reserve, in px, read from the computed CSS var (not any literal) so the
/// assertion tracks desktop.css / the ShellInit snapshot automatically and can't go stale.
async function reservePx(page: import("@playwright/test").Page): Promise<number> {
  const raw = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--traffic-light-reserve").trim()
  );
  const n = Number.parseFloat(raw);
  expect(n, `--traffic-light-reserve should be a positive px (got "${raw}")`).toBeGreaterThan(0);
  return n;
}

test("boots the desktop shell through the invoke bridge + desktop.css", async ({ page }) => {
  await page.goto("/index.tauri.html");
  // The native init-script's traits (mirrored by the stub) drive the macOS chrome — NOT UA sniffing.
  await expect(page.locator("html")).toHaveAttribute("data-os", "macos");
  await expect(page.locator("html")).toHaveAttribute("data-material", "vibrancy");
  // mountApp cleared the static startup skeleton and mounted the real shell — proving bootTauri.ts
  // reached the Rust core through the stubbed invoke transport (not the HTTP client).
  await expect(page.locator("[data-startup-shell]")).toHaveCount(0);
  await expect(page.locator(".shell .sidebar")).toBeVisible();
  await expect(page.locator(".brand .brand-name")).toHaveText("TARE");
});

test("pins the traffic-light seam as engine-independent geometry (default + min size)", async ({ page }) => {
  await page.goto("/index.tauri.html");
  const brand = page.locator(".brand .brand-name");

  // At the 1100×720 default the shell is the 60px icon rail. That column sits beneath the native
  // traffic lights, so its wordmark is intentionally hidden and the toolbar's first real content must
  // clear the measured reserve. This is the exact seam the reserve exists to protect.
  const reserve = await reservePx(page);
  await expect(brand).toBeHidden();
  const leadDefault = await page.locator(".topbar-lead").boundingBox();
  expect(leadDefault, "toolbar lead must be laid out").not.toBeNull();
  expect(leadDefault!.x).toBeGreaterThanOrEqual(reserve - 1);

  // At the real desktop minimum window (min_inner_size 420×520, gui.rs build_main_window) the reserve
  // still holds. The full-width brand row returns in single-pane mode and must not tuck its wordmark
  // under the lights.
  await page.setViewportSize({ width: 420, height: 520 });
  await expect(brand).toBeVisible();
  const boxMin = await brand.boundingBox();
  expect(boxMin, "brand wordmark must stay laid out at min width").not.toBeNull();
  expect(boxMin!.x).toBeGreaterThanOrEqual((await reservePx(page)) - 1);
});

test("collapses the traffic-light reserve in native fullscreen", async ({ page }) => {
  await page.goto("/index.tauri.html");
  const mark = page.locator(".brand .brand-mark");
  const lead = page.locator(".topbar-lead");
  const reserve = await reservePx(page);
  await expect(mark).toBeHidden();
  expect((await lead.boundingBox())!.x).toBeGreaterThanOrEqual(reserve - 1);

  // The Rust resize event reports fullscreen → bootTauri sets data-fullscreen → the reserve collapses
  // to the ordinary inset (macOS auto-hides the controls, so the inset would be dead space) and the
  // centered calibration mark can use the icon rail again.
  await page.evaluate(() => (window as unknown as { __tauriWindow: { setFullscreen(v: boolean): void } }).__tauriWindow.setFullscreen(true));
  await expect(page.locator("html")).toHaveAttribute("data-fullscreen", "true");
  await expect(mark).toBeVisible();
  await expect.poll(async () => (await lead.boundingBox())!.x).toBeLessThan(reserve);

  // Leaving fullscreen restores the reserve.
  await page.evaluate(() => (window as unknown as { __tauriWindow: { setFullscreen(v: boolean): void } }).__tauriWindow.setFullscreen(false));
  await expect(page.locator("html")).not.toHaveAttribute("data-fullscreen", "true");
  await expect(mark).toBeHidden();
  await expect.poll(async () => (await lead.boundingBox())!.x).toBeGreaterThanOrEqual(reserve - 1);
});

test("quiets the chrome when the window loses focus", async ({ page }) => {
  await page.goto("/index.tauri.html");
  await expect(page.locator("html")).not.toHaveAttribute("data-inactive", "true");
  await page.evaluate(() => (window as unknown as { __tauriWindow: { setFocus(v: boolean): void } }).__tauriWindow.setFocus(false));
  await expect(page.locator("html")).toHaveAttribute("data-inactive", "true");
  await page.evaluate(() => (window as unknown as { __tauriWindow: { setFocus(v: boolean): void } }).__tauriWindow.setFocus(true));
  await expect(page.locator("html")).not.toHaveAttribute("data-inactive", "true");
});

test("bridges the native command palette + find events into the WebView", async ({ page }) => {
  await page.goto("/index.tauri.html");
  // View ▸ Command Palette (⌘K) emits `open-palette`; bootTauri re-dispatches tare:open-palette.
  await page.evaluate(() => (window as unknown as { __tauriEmit(e: string, p?: unknown): void }).__tauriEmit("open-palette"));
  await expect(page.locator(".palette-overlay")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".palette-overlay")).toHaveCount(0);

  // Edit ▸ Find (⌘F) emits `find`; bootTauri re-dispatches tare:find → the in-webview find bar.
  await page.evaluate(() => (window as unknown as { __tauriEmit(e: string, p?: unknown): void }).__tauriEmit("find"));
  await expect(page.locator("#tare-find-bar")).toBeVisible();
});

test("navigates from a native Go-menu / tray event", async ({ page }) => {
  await page.goto("/index.tauri.html");
  await expect(page.locator(".shell .sidebar")).toBeVisible(); // shell up (fresh launch lands on onboarding)
  // The Rust menu/tray emits `navigate` with {route}; bootTauri turns it into in-app navigation,
  // regardless of the current route.
  await page.evaluate(() => (window as unknown as { __tauriEmit(e: string, p?: unknown): void }).__tauriEmit("navigate", { route: "investigate" }));
  await expect(page).toHaveURL(/#\/investigate/);

  // The tray's stable run payload is canonicalized by the WebView bridge; it must not strand the
  // user on the one-release `#/runs/:id` compatibility route with no active workspace context.
  await page.evaluate(() =>
    (window as unknown as { __tauriEmit(e: string, p?: unknown): void }).__tauriEmit("navigate", {
      route: "runs",
      param: "run-profile-fixture",
    })
  );
  await expect(page).toHaveURL(/#\/investigate\/run\/run-profile-fixture/);
  await expect(page.locator(".run-profile")).toBeVisible();
});

test("tints affordances from the OS system accent", async ({ page }) => {
  // The OS-accent bridge is opt-in (brass is the committed brand accent), so enable the pref before boot.
  await page.addInitScript(() => {
    try {
      localStorage.setItem("tare-os-accent", "on");
    } catch {
      /* sandbox without storage — the assertion below will surface it */
    }
  });
  await page.goto("/index.tauri.html");
  // Rust emits `system-accent` with the OS accent hex; wireSystemAccent clamps it to the AA floor and
  // repoints --accent-system / --focus-ring at it (data/chart ramps keep --accent, staying brand-stable).
  // The CSS carries a default --accent-system, so prove the EVENT re-tints by asserting it CHANGES.
  const before = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--accent-system").trim()
  );
  await page.evaluate(() => (window as unknown as { __tauriEmit(e: string, p?: unknown): void }).__tauriEmit("system-accent", "#e01e5a"));
  await expect
    .poll(async () =>
      page.evaluate(() =>
        getComputedStyle(document.documentElement).getPropertyValue("--accent-system").trim()
      )
    )
    .not.toBe(before);
  const after = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--accent-system").trim()
  );
  expect(after).not.toBe("");
});
