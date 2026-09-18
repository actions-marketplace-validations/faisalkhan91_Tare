// Optimize lifecycle and utility-sheet journeys use a deterministic, per-test in-memory fixture. The
// fixture records exact request DTOs but remains local-only: no model execution, payload read, live
// backend, notification delivery, or account/network boundary is involved.

import { expect, test } from "@playwright/test";
import { cohortHash } from "../src/analysis/serialize.js";
import type { CohortSpec } from "../src/analysis/types.js";
import { installFixtures } from "./fixtures.js";

test("inspect → apply → verify the same matched cohort, then remove/dismiss/restore", async ({ page }) => {
  const fixture = await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/optimize?investigation=matched-nightly&view=open");

  const row = page.locator(".optimize-lifecycle-row");
  await expect(row).toContainText("Right-size the nightly classifier");
  await expect(row).toContainText("Exact cohort · 2026-07-01–2026-07-07");
  await row.getByRole("button", { name: "Inspect evidence" }).click();
  await expect(row.locator(".optimize-evidence-inspector")).toContainText("4 affected runs");

  await row.getByRole("button", { name: "Mark applied" }).click();
  await page.locator('[data-lifecycle-view="verifying"]').click();
  await expect(page.locator(".optimize-lifecycle-row")).toContainText("Verifying");
  await expect(page.locator(".optimize-verification")).toContainText("Incomplete window");
  await expect(page.locator(".optimize-verification")).toContainText("No observed outcome is claimed yet");

  const afterApply = fixture.requestLog();
  expect(afterApply.accept).toHaveLength(1);
  const acceptedCohort = afterApply.accept[0].cohort as CohortSpec;
  expect(acceptedCohort.filters).toEqual([
    { op: "eq", dimension: "workload_key", value: "nightly" },
  ]);
  expect(afterApply.verify[0]).toMatchObject({
    opportunity_key: "rightsizing:nightly-classifier",
    cohort_hash: cohortHash(acceptedCohort),
  });

  fixture.completeVerification();
  await page.locator('[data-lifecycle-view="observed-reduction"]').click();
  const observed = page.locator(".optimize-lifecycle-row");
  await expect(observed.locator(".optimize-state")).toContainText("Observed reduction");
  await expect(observed.locator(".optimize-verification")).toContainText("Complete window");
  await expect(observed.locator(".optimize-verification")).toContainText(
    "$2.00 in the stored matched cohort"
  );
  await expect(observed.locator(".optimize-verification")).toContainText(
    "does not by itself establish causality"
  );

  await observed.getByRole("button", { name: "Remove applied state" }).click();
  await page.locator('[data-lifecycle-view="open"]').click();
  const reopened = page.locator(".optimize-lifecycle-row");
  await expect(reopened).toContainText("Right-size the nightly classifier");
  await reopened.getByRole("button", { name: "Dismiss" }).click();
  await page.locator('[data-lifecycle-view="dismissed"]').click();
  const dismissed = page.locator(".optimize-lifecycle-row");
  await expect(dismissed.locator(".optimize-state")).toContainText("Dismissed");
  await dismissed.getByRole("button", { name: "Restore to Open" }).click();
  await page.locator('[data-lifecycle-view="open"]').click();
  await expect(page.locator(".optimize-lifecycle-row .optimize-state")).toContainText("Open");

  const completed = fixture.requestLog();
  expect(completed.unaccept).toHaveLength(2);
  expect(completed.dismiss).toHaveLength(1);
  expect(completed.dismiss[0].cohort).toEqual(acceptedCohort);
});

test("scoped offline scenario stays counts-only and labels approximation/unpriced omissions", async ({ page }) => {
  const fixture = await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/optimize?investigation=matched-nightly&view=scenarios");

  await expect(page.locator(".optimize-scenarios")).toContainText(
    "never calls a model or reads captured payload text"
  );
  await page.locator("#scenario-models").fill("claude-haiku-4-5");
  await page.locator("[data-scenario-run]").click();

  const results = page.locator(".scenario-results");
  await expect(results).toContainText("Counterfactual estimates");
  await expect(results).toContainText("Estimated reduction");
  await expect(results).toContainText("$6.00");
  await expect(results).toContainText("Estimated from captured usage counts");
  await expect(results).toContainText("Approximate tokenizer reprice");
  await expect(results).toContainText("Unpriced targets are omitted, never shown as $0");

  const request = fixture.requestLog().experiment[0];
  expect(request.cohort).toMatchObject({
    timezone: "America/Los_Angeles",
    filters: [{ op: "eq", dimension: "workload_key", value: "nightly" }],
  });
  expect(JSON.stringify(request)).not.toContain("payload");
});

test("stopped capture → sheet remedy/self-check → flowing Pulse, with modal focus restored", async ({ page }) => {
  const fixture = await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/pulse");
  await page.locator(".attn-capture").click();
  await expect(page).toHaveURL(/(?:\?|&)sheet=capture/);

  const dialog = page.getByRole("dialog", { name: "Capture" });
  await expect(dialog).toBeVisible();
  await expect(dialog.locator(".utility-sheet-body")).toHaveAttribute("data-capture-state", "blind");
  await expect(dialog).toContainText("Agent activity is visible, but no cost events are captured");
  await expect(page.locator(".pulse")).toHaveAttribute("inert", "");
  for (const selector of [".brand", ".topbar", ".sidebar", ".statusbar"]) {
    await expect(page.locator(selector)).toHaveAttribute("inert", "");
  }

  // The sheet is diagnostic. Model the recommended local remedy landing, then rerun its read-only
  // self-check and require every surfaced claim to come from the refreshed local state.
  fixture.recoverCapture();
  await dialog.getByRole("button", { name: "Run self-check" }).click();
  await expect(dialog.locator(".capture-checks")).toContainText("12 captured events observed");
  await expect(dialog.locator(".capture-checks")).not.toContainText(/complete capture|100%/i);

  await dialog.getByRole("button", { name: "Close Capture" }).click();
  await expect(page).not.toHaveURL(/sheet=capture/);
  const trustStrip = page.locator(".pulse-trust");
  await expect(trustStrip).toContainText("Capture healthy");
  const coverageLink = trustStrip.getByRole("link", {
    name: "Review capture coverage and pricing",
  });
  await expect(coverageLink).toHaveText("Coverage");
  await expect(coverageLink).toHaveAttribute("href", /(?:\?|&)sheet=trust(?:&|$)/);
  await expect(page.locator('[data-utility-sheet-trigger="capture"]')).toBeFocused();
  for (const selector of [".brand", ".topbar", ".sidebar", ".statusbar"]) {
    await expect(page.locator(selector)).not.toHaveAttribute("inert", "");
  }
});

test("Trust/Pricing remains a scoped accessible sheet and keeps unpriced usage outside totals", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/pulse");

  const trigger = page.locator('[data-utility-sheet-trigger="trust"]');
  await trigger.click();
  let dialog = page.getByRole("dialog", { name: "Trust & pricing" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Channel health does not prove complete capture");
  await expect(dialog).toContainText("98% of captured tokens are priced");
  await expect(dialog).toContainText("remaining 2% is usage-only");
  await expect(dialog).toContainText("Reconciliation unavailable");

  await dialog.getByRole("link", { name: "Pricing catalog" }).click();
  dialog = page.getByRole("dialog", { name: "Trust & pricing" });
  await expect(dialog).toContainText("claude-haiku-4-5");
  await expect(dialog).toContainText("local/future-model");
  await expect(dialog).toContainText("not included in totals");
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(page.locator('[data-utility-sheet-trigger="trust"]')).toBeFocused();
});

test("System follows live OS appearance; explicit override wins; returning to System resumes it", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.addInitScript(() => localStorage.setItem("tare-theme", "system"));
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/index.html#/pulse?sheet=settings");

  const dialog = page.getByRole("dialog", { name: "Settings" });
  const appearance = dialog.locator('select:has(option[value="system"]):has(option[value="dark"])');
  await expect(appearance).toHaveValue("system");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await appearance.selectOption("dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await appearance.selectOption("system");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(page.locator(".topbar .icon-btn")).toHaveCount(0);
});

test("revised onboarding progresses Capture → first spend → optional Tune without a provider wall", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.addInitScript(() => {
    localStorage.removeItem("tare-onboarded");
    localStorage.removeItem("tare-onboard-step");
  });
  await page.goto("/index.html#/onboarding");

  await expect(page.getByRole("heading", { name: "1 · Captured history found" })).toBeVisible();
  await expect(page.locator(".onboard")).toContainText("No account is required, and nothing leaves this machine");
  await expect(page.locator(".onboard-optional")).not.toHaveAttribute("open", "");
  await page.getByRole("button", { name: "Next → See spend" }).click();
  await expect(page.locator(".onboard-wait")).toContainText("Found your sessions");
  await page.getByRole("button", { name: "Skip to tuning →" }).click();
  await expect(page.getByRole("heading", { name: "3 · Tune (optional): cap + privacy" })).toBeVisible();
  await page.getByRole("button", { name: "Skip, use defaults" }).click();
  await expect(page).toHaveURL(/#\/pulse/);
  await expect(page.locator(".pulse")).toBeVisible();
});
