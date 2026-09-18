import { expect, test } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test("utility sheets animate from their edge and finish closing before teardown", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/pulse");

  const trigger = page.locator('[data-utility-sheet-trigger="trust"]');
  await trigger.click();
  const backdrop = page.locator(".utility-sheet-backdrop");
  const dialog = page.locator(".utility-sheet");
  await expect(backdrop).toHaveAttribute("data-motion-state", "open");
  const timing = await dialog.evaluate((node) => {
    const style = getComputedStyle(node);
    return {
      properties: style.transitionProperty,
      durations: style.transitionDuration,
      opacity: style.opacity,
    };
  });
  expect(timing.properties).toContain("transform");
  expect(timing.durations).toContain("0.24s");
  expect(timing.opacity).toBe("1");
  const stableScrim = await backdrop.evaluate((node) => getComputedStyle(node).backgroundColor);

  await dialog.getByRole("button", { name: "Close Trust & pricing" }).click();
  await expect(backdrop).toHaveAttribute("data-motion-state", "closing");
  expect(await backdrop.evaluate((node) => getComputedStyle(node).backgroundColor)).toBe(stableScrim);
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
  await expect(backdrop).toHaveAttribute("data-motion-state", "closing");
  await expect(page.locator(".brand")).toHaveAttribute("inert", "");

  await expect(backdrop).toHaveCount(0);
  await expect(page.locator(".brand")).not.toHaveAttribute("inert", "");
  await expect(trigger).toBeFocused();
});

test("reduced motion removes edge-panel transitions without changing behavior", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/index.html#/pulse");

  const trigger = page.locator('[data-utility-sheet-trigger="capture"]');
  await trigger.click();
  const dialog = page.locator(".utility-sheet");
  await expect(dialog).toBeVisible();
  expect(await dialog.evaluate((node) => getComputedStyle(node).transitionProperty)).toBe("none");
  await dialog.getByRole("button", { name: "Close Capture" }).click();
  await expect(dialog).toHaveCount(0);
  await expect(trigger).toBeFocused();
});

test("responsive navigation uses the mirrored left-edge motion", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.setViewportSize({ width: 420, height: 760 });
  await page.goto("/index.html#/pulse");

  const shell = page.locator(".shell");
  const trigger = page.getByRole("button", { name: "Open navigation" });
  await trigger.click();
  await expect(shell).toHaveAttribute("data-nav-drawer-state", "open");
  const sidebar = page.locator(".sidebar");
  expect(await sidebar.evaluate((node) => getComputedStyle(node).transitionDuration)).toContain("0.24s");
  expect(await sidebar.evaluate((node) => getComputedStyle(node).opacity)).toBe("1");
  const backdrop = page.locator(".nav-drawer-backdrop");
  const stableScrim = await backdrop.evaluate((node) => getComputedStyle(node).backgroundColor);

  await page.getByRole("button", { name: "Close navigation" }).click();
  await expect(shell).toHaveAttribute("data-nav-drawer-state", "closing");
  expect(await backdrop.evaluate((node) => getComputedStyle(node).backgroundColor)).toBe(stableScrim);
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())));
  await expect(shell).toHaveAttribute("data-nav-drawer-state", "closing");
  await expect(shell).not.toHaveClass(/nav-drawer-open/);
  await expect(shell).not.toHaveAttribute("data-nav-drawer-state");
  await expect(trigger).toBeFocused();
});
