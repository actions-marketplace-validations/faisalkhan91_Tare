// Route redirect / canonical-workspace journey. Canonical routes render real content and legacy
// routes continue to resolve through the compatibility redirects.

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

const CANONICAL = [
  "#/pulse",
  "#/investigate?entity=sessions",
  "#/investigate?mode=timeline",
  "#/optimize",
];

for (const hash of CANONICAL) {
  test(`canonical route ${hash} renders an adapter, not a placeholder`, async ({ page }) => {
    await page.goto(`/index.html${hash}`);
    const main = page.locator(".main");
    await expect(main).toBeVisible();
    await expect(main).not.toContainText(/page not found/i);
    // The shell chrome is intact around it.
    await expect(page.locator(".shell .sidebar")).toBeVisible();
  });
}

type RouteCase = { source: string; path: string; query?: Record<string, string> };

const COMPATIBILITY_ROUTES: RouteCase[] = [
  { source: "#/live", path: "#/pulse", query: { mode: "now" } },
  { source: "#/overview", path: "#/pulse" },
  { source: "#/runs", path: "#/investigate", query: { entity: "runs" } },
  { source: "#/runs/run-profile-fixture", path: "#/investigate/run/run-profile-fixture" },
  { source: "#/sessions", path: "#/investigate", query: { entity: "sessions" } },
  { source: "#/trends?by=model", path: "#/investigate", query: { by: "model", mode: "timeline" } },
  { source: "#/segments", path: "#/investigate", query: { view: "facets" } },
  { source: "#/correlate", path: "#/investigate", query: { mode: "distinguish" } },
  { source: "#/lineage", path: "#/investigate", query: { entity: "templates", mode: "lineage" } },
  { source: "#/units", path: "#/investigate", query: { view: "units", norm: "per_outcome" } },
  { source: "#/compare?runs=run-a,run-b", path: "#/investigate/compare", query: { runs: "run-a,run-b" } },
  { source: "#/diff?runs=run-a,run-b", path: "#/investigate/compare", query: { runs: "run-a,run-b" } },
  { source: "#/experiments", path: "#/optimize", query: { view: "scenarios" } },
  { source: "#/whatif", path: "#/optimize", query: { view: "scenarios" } },
  { source: "#/advise", path: "#/optimize", query: { type: "cache" } },
  { source: "#/receipts", path: "#/pulse", query: { sheet: "trust" } },
  { source: "#/pricing", path: "#/pulse", query: { sheet: "trust", view: "pricing" } },
  { source: "#/connect", path: "#/pulse", query: { sheet: "capture" } },
  { source: "#/settings", path: "#/pulse", query: { sheet: "settings" } },
];

const CANONICAL_SURFACES: RouteCase[] = [
  { source: "#/pulse", path: "#/pulse" },
  { source: "#/investigate", path: "#/investigate" },
  { source: "#/investigate?mode=timeline", path: "#/investigate", query: { mode: "timeline" } },
  { source: "#/investigate?mode=distinguish", path: "#/investigate", query: { mode: "distinguish" } },
  { source: "#/investigate?entity=templates&mode=lineage", path: "#/investigate", query: { mode: "lineage" } },
  { source: "#/investigate?view=units", path: "#/investigate", query: { view: "units" } },
  { source: "#/investigate/run/run-profile-fixture?view=profile", path: "#/investigate/run/run-profile-fixture", query: { view: "profile" } },
  { source: "#/investigate/compare?runs=run-a,run-b", path: "#/investigate/compare", query: { runs: "run-a,run-b" } },
  { source: "#/optimize", path: "#/optimize" },
  { source: "#/optimize?view=scenarios", path: "#/optimize", query: { view: "scenarios" } },
  { source: "#/pulse?sheet=capture", path: "#/pulse", query: { sheet: "capture" } },
  { source: "#/pulse?sheet=trust", path: "#/pulse", query: { sheet: "trust" } },
  { source: "#/pulse?sheet=settings", path: "#/pulse", query: { sheet: "settings" } },
];

async function assertSurface(page: import("@playwright/test").Page, item: RouteCase): Promise<void> {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`/index.html${item.source}`);
  await expect.poll(() => page.evaluate(() => location.hash.split("?")[0])).toBe(item.path);
  if (item.query) {
    await expect.poll(() =>
      page.evaluate(() =>
        Object.fromEntries(new URLSearchParams(location.hash.split("?")[1] ?? ""))
      )
    ).toMatchObject(item.query);
  }
  await expect(page.locator(".main")).toBeVisible();
  await expect(page.locator(".main .skeleton")).toHaveCount(0);
  await expect(page.locator(".main .error:visible")).toHaveCount(0);
  await expect(page.locator(".main")).not.toContainText(/page not found/i);
  expect(errors).toEqual([]);
}

test("every canonical surface renders without a visible error", async ({ page }) => {
  test.setTimeout(60_000);
  for (const item of CANONICAL_SURFACES) {
    await test.step(item.source, () => assertSurface(page, item));
  }
});

test("every compatibility route resolves to a working canonical surface", async ({ page }) => {
  test.setTimeout(60_000);
  for (const item of COMPATIBILITY_ROUTES) {
    await test.step(item.source, () => assertSurface(page, item));
  }
});

test("a recent run opens the canonical profile with Investigate selected", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("tare-recent-runs", JSON.stringify(["run-profile-fixture"]));
  });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/pulse");

  const recent = page.locator('.nav-runs a[title="run-profile-fixture"]');
  await expect(recent).toBeVisible();
  await expect(recent).toHaveAttribute("href", "#/investigate/run/run-profile-fixture");
  await recent.click();

  await expect(page).toHaveURL(/#\/investigate\/run\/run-profile-fixture/);
  await expect(page.locator(".run-profile")).toBeVisible();
  await expect(page.locator('.sidebar a[aria-label="Investigate"]')).toHaveAttribute(
    "aria-current",
    "page"
  );
});
