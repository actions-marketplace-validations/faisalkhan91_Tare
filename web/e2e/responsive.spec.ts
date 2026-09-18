// Responsive real-pane journeys. Each normative width
// exercises the interaction unique to that mode, against the real Investigate panes.

import { test, expect } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

test("1100px: collapsed rail is a complete icon navigation, not clipped text", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 720 });
  await page.goto("/index.html#/pulse");
  const shell = page.locator(".shell");
  const rail = page.locator(".sidebar");
  await expect(shell).toHaveAttribute("data-layout", "rail-icons");
  await expect(rail).toBeVisible();

  const railBox = await rail.boundingBox();
  expect(railBox).not.toBeNull();
  expect(railBox!.width).toBe(60);
  await expect(page.locator(".brand-name")).toBeHidden();
  await expect(page.locator(".brand-mark")).toBeVisible();
  const collapsedText = rail.locator(".nav-section, .nav-item-label, .nav-commands-label");
  for (let index = 0; index < (await collapsedText.count()); index += 1) {
    await expect(collapsedText.nth(index)).toBeHidden();
  }
  const collapsedGroups = rail.locator(".nav-views, .nav-runs");
  for (let index = 0; index < (await collapsedGroups.count()); index += 1) {
    await expect(collapsedGroups.nth(index)).toBeHidden();
  }

  const destinations = rail.locator(".nav-destination");
  await expect(destinations).toHaveCount(6);
  for (let index = 0; index < (await destinations.count()); index += 1) {
    const destination = destinations.nth(index);
    await expect(destination.locator(".nav-item-icon")).toBeVisible();
    await expect(destination).toHaveAttribute("aria-label", /\S/);
    const box = await destination.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(railBox!.x);
    expect(box!.x + box!.width).toBeLessThanOrEqual(railBox!.x + railBox!.width + 0.5);
  }
  await expect(rail.locator(".nav-commands-icon")).toBeVisible();

  await rail.getByLabel("Investigate", { exact: true }).click();
  await expect(page).toHaveURL(/#\/investigate/);
  await expect(rail.getByLabel("Investigate", { exact: true })).toHaveAttribute("aria-current", "page");
});

async function openInvestigate(page: import("@playwright/test").Page, width: number, mode: string) {
  await page.setViewportSize({ width, height: 800 });
  await page.goto("/index.html#/investigate");
  await expect(page.locator(".shell")).toHaveAttribute("data-layout", mode);
  await expect(page.locator("[data-adaptive-panes]")).toBeVisible();
}

test("1280px: pointer + keyboard resize persist across reload", async ({ page }) => {
  await openInvestigate(page, 1280, "full");
  const entity = page.locator('[data-resize-pane="entities"]');
  const inspector = page.locator('[data-resize-pane="inspector"]');
  const start = Number(await entity.getAttribute("aria-valuenow"));
  const box = await entity.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + 30);
  await page.mouse.down();
  await page.mouse.move(box!.x + box!.width / 2 + 24, box!.y + 30);
  await page.mouse.up();
  await expect(entity).toHaveAttribute("aria-valuenow", String(start + 24));

  await inspector.focus();
  await page.keyboard.press("ArrowLeft");
  const savedEntity = await entity.getAttribute("aria-valuenow");
  const savedInspector = await inspector.getAttribute("aria-valuenow");
  expect(Number(savedInspector)).toBeGreaterThan(300);

  await page.reload();
  await expect(page.locator(".shell")).toHaveAttribute("data-layout", "full");
  await expect(page.locator('[data-resize-pane="entities"]')).toHaveAttribute("aria-valuenow", savedEntity!);
  await expect(page.locator('[data-resize-pane="inspector"]')).toHaveAttribute("aria-valuenow", savedInspector!);
});

test("900px: Space toggles inspector Peek", async ({ page }) => {
  await openInvestigate(page, 900, "rail-icons");
  const workspace = page.locator("[data-adaptive-panes]");
  const inspector = page.locator('[data-pane="inspector"]');
  await expect(inspector).toBeHidden();
  await page.keyboard.press("Space");
  await expect(workspace).toHaveAttribute("data-inspector-peek", "true");
  await expect(inspector).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(inspector).toBeHidden();
});

test("600px: Space Peek and Back/Forward entity-canvas stack both work", async ({ page }) => {
  await openInvestigate(page, 600, "stacked");
  const inspector = page.locator('[data-pane="inspector"]');
  await expect(page.locator("[data-pane-title]")).toHaveText("Filters");
  await page.keyboard.press("Space");
  await expect(inspector).toBeVisible();
  await page.keyboard.press("Space");
  await expect(inspector).toBeHidden();

  await page.locator("[data-pane-forward]").click();
  await expect(page.locator("[data-pane-title]")).toHaveText(/^Results/);
  await expect(page.locator('[data-pane="canvas"]')).toBeVisible();
  await page.locator("[data-pane-back]").click();
  await expect(page.locator("[data-pane-title]")).toHaveText("Filters");
});

test("330px: the one-pane Back/Forward stack reaches the inspector", async ({ page }) => {
  await openInvestigate(page, 330, "single");
  const title = page.locator("[data-pane-title]");
  const forward = page.locator("[data-pane-forward]");
  await expect(title).toHaveText("Filters");
  await forward.click();
  await expect(title).toHaveText(/^Results/);
  await forward.click();
  await expect(title).toHaveText("Explain");
  await expect(page.locator('[data-pane="inspector"]')).toBeVisible();
  await page.locator("[data-pane-back]").click();
  await expect(title).toHaveText(/^Results/);
});
