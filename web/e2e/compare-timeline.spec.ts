// Timeline and Compare journeys: matched single-run structural diff, multi-run baseline matrix with
// explicit-representative gating, compatibility warnings, normalization, and responsive keyboard
// flows. The fixture backend echoes the brushed window or navigated run pair. Every
// journey asserts a URL/state round trip so shareable links reconstruct the analysis, not in-memory
// state.

import { expect, test } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test.beforeEach(async ({ page }) => {
  await installFixtures(page);
});

test("matched single-run Compare shows summary, decomposition, cause diff, and structural flame diff; mode round-trips", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/investigate/compare?runs=run-a,run-b");

  // The summary matrix is the authoritative header.
  await expect(page.locator(".compare-summary-matrix")).toBeVisible();

  // Volume, size, and efficiency components sum exactly to the total delta with explicit units.
  const decomp = page.locator(".compare-decomposition");
  await expect(decomp).toBeVisible();
  await expect(decomp.locator('[data-metric="total"] .dollars')).toHaveText("+$5.00");
  await expect(decomp.locator('[data-sums-exactly="true"]')).toBeVisible();
  // Efficiency countervails the +total → flagged as a reversal, not recolored into a false severity.
  await expect(decomp.locator('[data-component="efficiency"][data-reversal="true"]')).toBeVisible();

  // The cause report diff is a distinct section from the decomposition.
  const cause = page.locator(".compare-cause-diff");
  await expect(cause).toBeVisible();
  await expect(cause.locator('[data-rank="increase"] .cause').first()).toHaveText("bigger-context");

  // A single run on each side auto-resolves the structural flame-diff pair.
  const flame = page.locator(".flame-diff");
  await expect(flame).toBeVisible();
  await expect(flame).toHaveAttribute("data-mode", "absolute");
  await expect(flame.locator('tr[data-path="0"]')).toContainText("costlier"); // tools +$5.00

  // Switch to normalized (share) mode → the choice lives in the URL and survives a reload.
  await flame.getByRole("button", { name: "Normalized (share)" }).click();
  await expect(page).toHaveURL(/flame_mode=normalized/);
  await expect(page.locator(".flame-diff")).toHaveAttribute("data-mode", "normalized");
  await page.reload();
  await expect(page.locator(".flame-diff")).toHaveAttribute("data-mode", "normalized");
});

test("multi-run Compare shows the matrix + compatibility warning and synthesizes no flame tree until an explicit pair is chosen", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/investigate/compare?runs=run-a,run-b,run-c");

  // Two candidate columns against the fixed Baseline B.
  await expect(page.locator(".compare-summary-matrix")).toBeVisible();
  // Compatibility warnings are always visible rather than silently changing the comparison.
  await expect(page.locator(".compare-warning").first()).toContainText("Workloads differ");

  // No aggregate flame tree for a multi-run cohort until the user picks a representative run.
  await expect(page.locator(".flame-diff")).toHaveCount(0);

  // Choosing a candidate representative resolves the pair → the structural flame diff appears.
  await page.getByLabel("Candidate representative run").selectOption("run-b");
  await expect(page.locator(".flame-diff")).toBeVisible();
  await expect(page).toHaveURL(/pair_candidate=run-b/);
});

test("timeline brush creates Selection A and the shareable URL round-trips it", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/investigate?mode=timeline&from=2026-07-01&to=2026-07-07&tz=UTC");

  const brush = page.locator(".inv-brush");
  await expect(brush).toBeVisible();

  // Brush days 2–4 (indices 1..3) into Selection A. Range inputs need an explicit input event.
  const setRange = async (selector: string, value: number): Promise<void> => {
    await page.locator(selector).evaluate((el, v) => {
      const input = el as HTMLInputElement;
      input.value = String(v);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    }, value);
  };
  await setRange(".inv-brush-start", 1);
  await setRange(".inv-brush-end", 3);
  await expect(page.locator(".inv-brush-output")).toContainText("2026-07-02–2026-07-04");

  await page.locator(".inv-brush-apply").click();
  // Applying resolves the brushed cohort + its prior-window baseline into Selection A / Baseline B and
  // writes them into the shareable hash. That hash change re-renders the timeline, which rehydrates the
  // brush from the persisted selection — so the range still reads 2026-07-02–2026-07-04 and the URL now
  // carries `sel=` (the transient "N runs" success line is expected to be replaced by this re-render).
  await expect(page).toHaveURL(/(?:\?|&)sel=/);
  await expect(page.locator(".inv-brush-output")).toContainText("Selection A · 2026-07-02–2026-07-04");

  // Round trip: the URL alone reconstructs the brushed selection after a reload.
  const urlWithSelection = page.url();
  await page.reload();
  expect(page.url()).toBe(urlWithSelection);
  await expect(page.url()).toContain("sel=");
});

test("Compare stays usable at 330px and its controls are keyboard-reachable", async ({ page }) => {
  await page.setViewportSize({ width: 330, height: 720 });
  await page.goto("/index.html#/investigate/compare?runs=run-a,run-b");

  // The authoritative sections still render in the narrowest supported width.
  await expect(page.locator(".compare-summary-matrix")).toBeVisible();
  await expect(page.locator(".compare-decomposition")).toBeVisible();
  await expect(page.locator(".flame-diff")).toBeVisible();

  // The flame-diff mode control is operable by keyboard alone (focus + Enter), not pointer-only.
  const normalizedBtn = page.locator(".flame-diff").getByRole("button", { name: "Normalized (share)" });
  await normalizedBtn.focus();
  await expect(normalizedBtn).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/flame_mode=normalized/);
});
