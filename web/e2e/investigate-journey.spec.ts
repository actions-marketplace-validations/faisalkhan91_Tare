// Pulse → Investigate → representative Run Profile journeys against the 1,000-row
// deterministic fixture, including durable context, privacy/timing fallbacks, keyboard selection,
// virtualization, and every normative responsive width.

import { expect, test } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

test("anomaly → cohort → factor/search → representative run → frame preserves scope and baseline", async ({ page }) => {
  await installFixtures(page);
  await page.addInitScript(() => localStorage.setItem("tare-baseline-run", "baseline-fixture"));
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/pulse?from=2026-07-01&to=2026-07-14&tz=America%2FLos_Angeles");

  const anomaly = page.locator(".attn-anomaly");
  await expect(anomaly).toContainText("claude-sonnet-4");
  await anomaly.click();
  await expect(page).toHaveURL(/#\/investigate\?/);
  await expect(page).toHaveURL(/(?:\?|&)mode=timeline/);
  await expect(page).toHaveURL(/(?:\?|&)sel=/);
  await expect(page).toHaveURL(/(?:\?|&)base=/);
  await expect(page.locator(".inv-timeline")).toBeVisible();

  // Remove the compatibility preference, then reload: the shareable URL alone must retain both
  // Selection A and Baseline B rather than relying on the originating in-memory store.
  await page.evaluate(() => localStorage.removeItem("tare-baseline-run"));
  await page.reload();
  const context = page.getByRole("group", { name: "Analysis context" });
  await expect(context).toContainText("Scope · 2026-07-01–2026-07-14 · America/Los_Angeles");
  await expect(context).toContainText("Selection A · 1 filter · 2026-07-14");
  // New baseline metadata companions preserve the original rule instead of degrading every shared
  // `base=` to an explicit cohort. This fixture starts from the compatibility pinned-run preference.
  await expect(context).toContainText("Baseline B · Pinned run");
  await expect(page.locator(".inv-filter")).toContainText("model = claude-sonnet-4");

  // The anomaly correctly opens Timeline first. Continue the evidence journey through Runs without
  // dropping Selection A or Baseline B.
  await page.getByRole("tab", { name: "Runs" }).click();

  // The full 1,000-row result is available to AT and keyboard navigation while the DOM remains
  // bounded to one viewport plus overscan (no more than approximately 80 rows).
  const resultTable = page.locator(".inv-results table");
  await expect(resultTable).toHaveAttribute("aria-rowcount", "1001");
  const renderedRows = await page.locator(".inv-results tbody tr:not(.dt-spacer)").count();
  expect(renderedRows).toBeLessThanOrEqual(80);

  await expect(page.locator(".insp-factors")).toContainText("association, not a proven cause");
  await expect(page.locator(".insp-factor-name").first()).toContainText("claude-sonnet-4");
  await page.locator(".inv-search").fill("run-profile-fixture");
  await expect(page.locator(".inv-search-status")).toContainText("1 match");
  await expect(page.locator('.inv-results [data-nav-id="run-profile-fixture"]')).toBeVisible();

  const representative = page.locator(".insp-rep").filter({ hasText: "Largest" }).getByRole("link");
  await expect(representative).toHaveAttribute("href", /sel=/);
  await expect(representative).toHaveAttribute("href", /base=/);
  await representative.click();
  await expect(page.locator(".run-profile")).toBeVisible();
  await expect(page).toHaveURL(/(?:\?|&)sel=/);
  await expect(page).toHaveURL(/(?:\?|&)base=/);

  const baseline = page.locator(".run-summary-datum").filter({ hasText: "Baseline delta" });
  await expect(baseline).not.toContainText("Not selected");
  await expect(baseline).toContainText("n=1000");

  const frame = page.locator('.run-profile-flame [data-frame-kind="component"]').first();
  await frame.focus();
  await page.keyboard.press("Enter");
  const selectedByNextFrame = await frame.evaluate(
    (node) => new Promise<boolean>((resolve) => requestAnimationFrame(() => resolve(node.getAttribute("aria-pressed") === "true")))
  );
  expect(selectedByNextFrame).toBe(true);
  await expect(frame).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".run-profile-profile-status")).toContainText(/call/);

  // The product Back path returns to the exact same analysis context, not an unscoped cohort.
  await page.locator(".run-profile-back").click();
  await expect(page.getByRole("group", { name: "Analysis context" })).toContainText("Baseline B · Pinned run");
  await expect(page.locator(".inv-filter")).toContainText("model = claude-sonnet-4");
});

test("counts-only profile never requests bodies and untimed capture is labelled Step order", async ({ page }) => {
  const apiPaths: string[] = [];
  await installFixtures(page, {
    stepOrderOnly: true,
    onApiRequest: (pathname) => apiPaths.push(pathname),
  });
  await page.goto("/index.html#/investigate/run/run-profile-fixture?view=shape");

  await expect(page.getByRole("tabpanel", { name: "Shape" })).toContainText("Prompt and response text are not read");
  await expect(page.getByRole("heading", { name: "Inspect stored bodies" })).toHaveCount(0);
  expect(apiPaths.some((path) => path.includes("transcript"))).toBe(false);

  await page.getByRole("tab", { name: "Provenance" }).click();
  await expect(page.getByRole("tabpanel", { name: "Provenance" })).toContainText("strict_counts");

  await page.getByRole("tab", { name: "Timeline" }).click();
  const panel = page.getByRole("tabpanel", { name: "Timeline" });
  await expect(panel.getByRole("heading", { name: "Step order" })).toBeVisible();
  await expect(panel).toContainText("no elapsed position or concurrency is implied");
  await expect(panel.locator("[data-concurrency-evidence]")).toHaveCount(0);
  expect(apiPaths.some((path) => path.includes("transcript"))).toBe(false);
});

test("local pane navigation stays below the stable interaction timing gates", async ({ page }) => {
  await installFixtures(page);
  await page.setViewportSize({ width: 760, height: 800 });
  await page.goto("/index.html#/investigate");
  await expect(page.locator(".inv-results table")).toHaveAttribute("aria-rowcount", "1001");

  await page.evaluate(() => {
    const state = window as Window & { __investigateLongTasks?: number[]; __investigateObserver?: PerformanceObserver };
    state.__investigateLongTasks = [];
    state.__investigateObserver?.disconnect();
    state.__investigateObserver = new PerformanceObserver((list) => {
      state.__investigateLongTasks!.push(...list.getEntries().map((entry) => entry.duration));
    });
    state.__investigateObserver.observe({ entryTypes: ["longtask"] });
  });

  const durations = await page.evaluate(async () => {
    const forward = document.querySelector<HTMLButtonElement>("[data-pane-forward]")!;
    const back = document.querySelector<HTMLButtonElement>("[data-pane-back]")!;
    const samples: number[] = [];
    const measure = async (button: HTMLButtonElement) => {
      const start = performance.now();
      button.click();
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      samples.push(performance.now() - start);
    };
    for (let index = 0; index < 5; index += 1) {
      await measure(forward);
      await measure(back);
    }
    return samples;
  });
  const sorted = [...durations].sort((a, b) => a - b);
  expect(sorted[Math.floor(sorted.length / 2)]).toBeLessThan(100);
  const longTasks = await page.evaluate(async () => {
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const state = window as Window & { __investigateLongTasks?: number[]; __investigateObserver?: PerformanceObserver };
    state.__investigateObserver?.disconnect();
    return state.__investigateLongTasks ?? [];
  });
  // Treat WebView timings as measured signals rather than zero-tolerance assertions: long tasks must
  // remain rare across the ten navigations and none may become a multi-hundred-millisecond stall.
  // The median-under-100ms assertion above is the stable interaction metric.
  expect(longTasks.length, `long tasks too common: ${longTasks.join(", ")}`).toBeLessThanOrEqual(2);
  expect(Math.max(0, ...longTasks), `a navigation janked badly: ${longTasks.join(", ")}`).toBeLessThan(150);
});

const WIDTHS = [
  [330, "single"],
  [420, "single"],
  [500, "single"],
  [760, "stacked"],
  [1100, "rail-icons"],
  [1440, "full"],
  [1800, "full"],
] as const;

for (const [width, mode] of WIDTHS) {
  test(`${width}px panes remain reachable in ${mode} layout`, async ({ page }) => {
    await installFixtures(page);
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/index.html#/investigate");
    await expect(page.locator(".shell")).toHaveAttribute("data-layout", mode);

    const entities = page.locator('[data-pane="entities"]');
    const canvas = page.locator('[data-pane="canvas"]');
    const inspector = page.locator('[data-pane="inspector"]');
    if (mode === "single") {
      await expect(page.locator("[data-pane-title]")).toHaveText("Filters");
      await page.locator("[data-pane-forward]").click();
      await expect(canvas).toBeVisible();
      await page.locator("[data-pane-forward]").click();
      await expect(inspector).toBeVisible();
      await page.locator("[data-pane-back]").click();
      await expect(canvas).toBeVisible();
    } else if (mode === "stacked") {
      await expect(entities).toBeVisible();
      await page.locator("[data-pane-forward]").click();
      await expect(canvas).toBeVisible();
      await page.keyboard.press("Space");
      await expect(inspector).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(inspector).toBeHidden();
    } else if (mode === "rail-icons") {
      await expect(entities).toBeVisible();
      await expect(canvas).toBeVisible();
      await expect(inspector).toBeHidden();
      await page.keyboard.press("Space");
      await expect(inspector).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(inspector).toBeHidden();
    } else {
      await expect(entities).toBeVisible();
      await expect(canvas).toBeVisible();
      await expect(inspector).toBeVisible();
    }
  });
}
