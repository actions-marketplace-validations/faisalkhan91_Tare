// Keyboard command journey. The global shortcut opens the contextual
// command palette; Escape closes it; the rail Commands row opens the same surface.

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

test("Ctrl/Cmd+K opens the command palette and Escape closes it", async ({ page }) => {
  await page.goto("/index.html#/pulse");
  await expect(page.locator(".shell")).toBeVisible();
  await page.keyboard.press("Control+k");
  const palette = page.locator(".palette");
  await expect(palette).toBeVisible();
  // The palette is a searchable command list.
  await expect(palette.locator("input")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(palette).toHaveCount(0);
});

test("the rail Commands row opens the same palette (single menu surface)", async ({ page }) => {
  await page.goto("/index.html#/pulse");
  const commands = page.locator(".nav-commands");
  await expect(commands).toBeVisible();
  await expect(commands).toHaveAttribute("data-action", "action:open-palette");
  await commands.click();
  await expect(page.locator(".palette")).toBeVisible();
});
