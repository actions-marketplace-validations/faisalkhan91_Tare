// Analysis-state persistence journey. The shipped URL codec round-trips
// analysis state (incl. Unicode) in the actual browser, and a view-state query survives a reload.

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

test("the analysis URL codec round-trips state (incl. Unicode) in the browser", async ({ page }) => {
  await page.goto("/index.html#/pulse");
  // Exercise the SHIPPED serialize.js module in page context — the same code the shell uses.
  const roundTrips = await page.evaluate(async () => {
    // This is an HTTP-root module URL inside the built page, not a Node-resolvable test import.
    const shippedModuleUrl = "/analysis/serialize.js";
    const m = await import(shippedModuleUrl);
    const filters = [{ op: "eq", dimension: "session", value: "café ☕ 日本語" }];
    const decoded = m.decodeFilters(m.encodeFilters(filters));
    return decoded[0].value === "café ☕ 日本語";
  });
  expect(roundTrips).toBe(true);
});

test("a view-state query survives a reload", async ({ page }) => {
  const state = () =>
    page.evaluate(() =>
      Object.fromEntries(new URLSearchParams(location.hash.split("?")[1] ?? ""))
    );
  await page.goto("/index.html#/trends?by=model");
  await expect(page.locator(".main")).toBeVisible();
  await expect(page).toHaveURL(/#\/investigate\?/);
  await expect.poll(state).toMatchObject({ by: "model", mode: "timeline" });
  await page.reload();
  // The query is preserved in the URL after re-render (deep-linkable/reloadable view state).
  await expect(page).toHaveURL(/#\/investigate\?/);
  await expect.poll(state).toMatchObject({ by: "model", mode: "timeline" });
});
