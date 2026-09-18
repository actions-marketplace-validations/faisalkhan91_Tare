// Startup + no-flash smoke journey. Verifies the pre-paint theme, the
// static startup shell → real shell handoff, and the first-paint instrumentation on the built assets.

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

test("boots with a resolved theme (no flash) and mounts the real shell", async ({ page }) => {
  await page.goto("/index.html");
  // data-theme is resolved by the render-blocking prepaint script before paint.
  const theme = await page.locator("html").getAttribute("data-theme");
  expect(["light", "dark"]).toContain(theme);
  // The static startup skeleton has been replaced by the real shell.
  await expect(page.locator("[data-startup-shell]")).toHaveCount(0);
  await expect(page.locator(".shell .sidebar")).toBeVisible();
  await expect(page.locator(".statusbar")).toHaveAttribute("aria-label", "Workspace status");
});

test("instruments the shell's first paint", async ({ page }) => {
  await page.goto("/index.html");
  await expect(page.locator(".shell")).toBeVisible();
  const marks = await page.evaluate(() =>
    performance.getEntriesByName("tare:shell-ready").length
  );
  expect(marks).toBeGreaterThan(0);
});
