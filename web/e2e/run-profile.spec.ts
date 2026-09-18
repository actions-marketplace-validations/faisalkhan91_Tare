import { expect, test } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test("Run Profile keeps summary fixed, context bounded, and preserves investigation through tabs/back", async ({ page }) => {
  await installFixtures(page);
  await page.goto(
    "/#/investigate/run/run-profile-fixture?from=2026-07-01&to=2026-07-14&tz=America%2FLos_Angeles&model=claude-sonnet-4&view=profile"
  );

  const profile = page.locator(".run-profile");
  await expect(profile).toBeVisible();
  await expect(page.locator(".master-detail, .statement-detail")).toHaveCount(0);
  await expect(page.locator(".run-profile-summary")).toContainText("$2.40");
  await expect(page.locator(".run-profile-summary")).toContainText("91");
  await expect(page.getByRole("tab")).toHaveCount(4);
  await expect(page.getByRole("complementary", { name: "Persistent run context" })).toContainText("Attestation");
  const breadcrumbBack = page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: "Investigate" });
  await expect(breadcrumbBack).toHaveAttribute("href", /from=2026-07-01/);
  await expect(breadcrumbBack).toHaveAttribute("href", /model=claude-sonnet-4/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);

  expect(await page.locator(".run-profile-header").evaluate((node) => getComputedStyle(node).position)).toBe("sticky");
  expect(await page.locator(".run-profile-inspector").evaluate((node) => getComputedStyle(node).overflowY)).toBe("auto");
  expect(await page.locator(".main").evaluate((node) => getComputedStyle(node).overflow)).toBe("hidden");

  await page.getByRole("tab", { name: "Shape" }).click();
  await expect(page).toHaveURL(/view=shape/);
  await expect(page.getByRole("tabpanel", { name: "Shape" })).toContainText("opaque-system-hash");

  await page.locator(".run-profile-back").click();
  await expect(page).toHaveURL(/#\/investigate\?/);
  await expect(page).toHaveURL(/from=2026-07-01/);
  await expect(page).toHaveURL(/model=claude-sonnet-4/);
  expect(page.url()).not.toContain("view=shape");
  await expect(page.locator(".main")).not.toHaveClass(/main-bounded-workbench/);
});

test("true timestamp Timeline keeps step selection cross-highlighted through Profile", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/investigate/run/run-profile-fixture?view=timeline");

  await expect(page.getByRole("tabpanel", { name: "Timeline" }).getByRole("heading", { name: "Timeline" })).toBeVisible();
  await expect(page.locator("[data-concurrency-evidence=confirmed]")).toContainText("shared-parent evidence");
  await page.locator('[data-step-ordinal="2"] .run-profile-step-select').click();
  await expect(page.locator(".run-profile-selected-step")).toContainText("Selected Step 2");
  await expect(page.locator('[data-beam-key="cache_read"].is-cross-highlighted')).not.toHaveCount(0);

  await page.getByRole("tab", { name: "Profile" }).click();
  await expect(page).toHaveURL(/view=profile/);
  await expect(page.locator('.run-profile-flame [data-step-ordinal="2"][aria-pressed="true"]')).not.toHaveCount(0);
  await expect(page.locator('tr[data-step-ordinals="2"].is-cross-highlighted').first()).toBeVisible();
  await expect(page.locator(".run-profile-selected-step")).toContainText("Selected Step 2");

  await page.getByRole("tab", { name: "Timeline" }).click();
  await expect(page.locator('[data-step-ordinal="2"] .run-profile-step-select')).toHaveAttribute("aria-pressed", "true");
});

test("Profile switches weight geometry, recursively aggregates, and requires a Sandwich component", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/investigate/run/run-profile-fixture?view=profile");

  const stepOne = page.locator('.run-profile-flame [data-frame-kind="step"][data-step-ordinal="1"]');
  const costWidth = Number(await stepOne.getAttribute("width"));
  await page.locator('[data-profile-weight="tokens"]').click();
  const tokenWidth = Number(await stepOne.getAttribute("width"));
  expect(tokenWidth).not.toBe(costWidth);

  await stepOne.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator(".run-profile-selected-step")).toContainText("Selected Step 1");

  await page.locator('[data-profile-order="aggregated"]').click();
  await expect(page.locator('.run-profile-flame [data-frame-kind="step"]')).toHaveCount(1);
  await expect(page.locator(".run-profile-flame")).toContainText("2 calls");
  await expect(page.locator(".run-profile-cost-table")).toContainText("Cost / call");

  await page.locator('[data-profile-order="sandwich"]').click();
  await expect(page.locator(".run-profile-flame")).toContainText("Choose a Sandwich component");
  const systemValue = await page
    .locator(".run-profile-component-select option")
    .filter({ hasText: "System" })
    .getAttribute("value");
  await page.locator(".run-profile-component-select").selectOption(systemValue!);
  await expect(page.locator(".run-profile-flame")).toContainText("Sandwich");
  await expect(page.locator(".run-profile-cost-table")).toContainText("Caller");
  await expect(page.locator(".run-profile-cost-table")).toContainText("Selected");
  await expect(page.locator(".run-profile-cost-table")).toContainText("Callee");
});
