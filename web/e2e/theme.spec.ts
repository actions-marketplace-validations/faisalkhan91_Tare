// Theme + state-persistence journey. System/Light/Dark lives only in Settings;
// an explicit choice applies immediately AND persists across a reload (localStorage state survives).

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

test("choosing Dark in Settings → Appearance applies and persists across reload", async ({ page }) => {
  await page.goto("/index.html#/settings");
  // The Appearance tri-state is the select carrying system/light/dark.
  const appearance = page.locator('select:has(option[value="system"]):has(option[value="dark"])');
  await expect(appearance).toBeVisible();
  await appearance.selectOption("dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  // State persistence: the preference survives a full reload (localStorage-backed).
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(
    page.locator('select:has(option[value="system"]):has(option[value="dark"])')
  ).toHaveValue("dark");
});

test("Light is an explicit override; the topbar/palette expose no theme toggle", async ({ page }) => {
  await page.goto("/index.html#/settings");
  const appearance = page.locator('select:has(option[value="system"]):has(option[value="dark"])');
  await appearance.selectOption("light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  // No topbar command/theme button remains.
  await expect(page.locator(".topbar .icon-btn")).toHaveCount(0);
});
