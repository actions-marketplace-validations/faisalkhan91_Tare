// Bounded Runs/Run Profile geometry and scroll journey. Exercises the exact
// 760/1100/1440 acceptance widths against built assets with a 160-run virtual list.

import { expect, test, type Page } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

async function openRun(page: Page, width: number, mode: string) {
  await page.setViewportSize({ width, height: 800 });
  await page.goto("/#/investigate/run/run-profile-fixture?view=profile");
  await expect(page.locator(".shell")).toHaveAttribute("data-layout", mode);
  await expect(page.locator(".run-profile[data-adaptive-panes]")).toBeVisible();
  await expect(page.locator(".master-detail, .statement-detail")).toHaveCount(0);
}

test("1440px: bounded panes use the window, scroll independently, and retain list focus/position", async ({ page }) => {
  await openRun(page, 1440, "full");
  const entities = page.locator('[data-pane="entities"]');
  const canvas = page.locator('[data-pane="canvas"]');
  const inspector = page.locator('[data-pane="inspector"]');
  const listScroll = page.locator(".run-profile-run-scroll");
  const header = page.locator(".run-profile-header");
  for (const pane of [entities, canvas, inspector]) await expect(pane).toBeVisible();

  const entityBox = (await entities.boundingBox())!;
  const canvasBox = (await canvas.boundingBox())!;
  const inspectorBox = (await inspector.boundingBox())!;
  expect(entityBox.width).toBeGreaterThanOrEqual(260);
  expect(entityBox.width).toBeLessThanOrEqual(320);
  expect(inspectorBox.width).toBeGreaterThanOrEqual(280);
  expect(inspectorBox.width).toBeLessThanOrEqual(340);
  expect(canvasBox.width).toBeGreaterThan(420);

  const headerY = (await header.boundingBox())!.y;
  await listScroll.evaluate((node) => {
    node.scrollTop = 900;
    node.dispatchEvent(new Event("scroll"));
  });
  await canvas.evaluate((node) => {
    node.scrollTop = 320;
    node.dispatchEvent(new Event("scroll"));
  });
  expect(await listScroll.evaluate((node) => node.scrollTop)).toBeGreaterThan(800);
  expect(await canvas.evaluate((node) => node.scrollTop)).toBeGreaterThan(0);
  expect(await inspector.evaluate((node) => node.scrollTop)).toBe(0);
  expect(await page.locator(".main").evaluate((node) => node.scrollTop)).toBe(0);
  expect((await header.boundingBox())!.y).toBe(headerY);

  // Pick a row inside the visible window, not one of the offscreen overscan rows (Playwright would
  // legitimately scroll an overscan row into view before clicking it, changing the expected value).
  const next = page.locator(".run-profile-run-link:not(.active)").nth(8);
  const nextId = await next.getAttribute("data-run-id");
  expect(nextId).toBeTruthy();
  const before = await listScroll.evaluate((node) => node.scrollTop);
  await next.click();
  await expect(page).toHaveURL(new RegExp(`/investigate/run/${nextId}`));
  // A hash URL changes before the async workspace render replaces the old pane. The clicked link in
  // that old pane is already focused, so a focus-only assertion can pass while the replacement
  // viewport is still at its initial scrollTop=0, one rAF before restoration. Pin the new render by
  // its heading first, then observe the post-layout focus/scroll contract.
  await expect(page.locator(".run-profile-header h2")).toHaveText(nextId!);
  await expect(page.locator(`.run-profile-run-link[data-run-id="${nextId}"]`)).toBeFocused();
  await expect.poll(async () => {
    const after = await page.locator(".run-profile-run-scroll").evaluate((node) => node.scrollTop);
    return Math.abs(after - before);
  }).toBeLessThan(4);
  expect(await page.locator(".main").evaluate((node) => node.scrollTop)).toBe(0);
});

test("1100px: filters + results stay fixed while Explain opens as Space/Escape Peek", async ({ page }) => {
  await openRun(page, 1100, "rail-icons");
  const workbench = page.locator(".run-profile");
  const canvas = page.locator('[data-pane="canvas"]');
  const inspector = page.locator('[data-pane="inspector"]');
  await expect(page.locator('[data-pane="entities"]')).toBeVisible();
  await expect(canvas).toBeVisible();
  await expect(inspector).toBeHidden();
  const canvasWidth = (await canvas.boundingBox())!.width;

  await page.locator('[role="tabpanel"]').focus();
  await page.keyboard.press("Space");
  await expect(workbench).toHaveAttribute("data-inspector-peek", "true");
  await expect(inspector).toBeVisible();
  expect((await canvas.boundingBox())!.width).toBe(canvasWidth);
  await page.keyboard.press("Escape");
  await expect(inspector).toBeHidden();
});

test("760px: selected run opens Results first and Back exposes the run list without stacking panes", async ({ page }) => {
  await openRun(page, 760, "stacked");
  const title = page.locator("[data-pane-title]");
  await expect(title).toHaveText("Results");
  await expect(page.locator('[data-pane="canvas"]')).toBeVisible();
  await expect(page.locator('[data-pane="entities"]')).toBeHidden();

  await page.locator("[data-pane-back]").click();
  await expect(title).toHaveText("Filters");
  const next = page.locator(".run-profile-run-link:not(.active)").first();
  const nextId = await next.getAttribute("data-run-id");
  await next.click();
  await expect(page).toHaveURL(new RegExp(`/investigate/run/${nextId}`));
  await expect(page.locator("[data-pane-title]")).toHaveText("Results");
  await expect(page.locator('[data-pane="entities"]')).toBeHidden();

  await page.locator('[role="tabpanel"]').focus();
  await page.keyboard.press("Space");
  await expect(page.locator('[data-pane="inspector"]')).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator('[data-pane="inspector"]')).toBeHidden();
});

test("bare compatibility Runs route opens canonical Investigate results", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 800 });
  await page.goto("/#/runs");
  await expect(page).toHaveURL(/#\/investigate\?(?:.*&)?entity=runs(?:&|$)/);
  await expect(page.locator('[data-pane="canvas"]')).toBeVisible();
  await expect(page.locator('[data-pane="entities"] input[type="search"]')).toBeVisible();
  await expect(page.locator("[data-adaptive-panes]")).toContainText("Runs");
  await expect(page.locator(".master-detail, .statement-detail")).toHaveCount(0);
});
