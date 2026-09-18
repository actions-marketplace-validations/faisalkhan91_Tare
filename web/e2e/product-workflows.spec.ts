// High-risk product journeys that are easy to miss in
// DOM-only tests — modal navigation, responsive reflow, authoritative range state, disclosure order,
// and first-run recovery. These run against the built app, not isolated components.

import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFixtures } from "./fixtures.js";

async function expectAccessibleState(page: Page, label: string): Promise<void> {
  const result = await new AxeBuilder({ page }).analyze();
  const violations = result.violations.map(({ id, impact, help, nodes }) => ({
    id,
    impact,
    help,
    targets: nodes.slice(0, 5).map((node) => node.target.join(" ")),
  }));
  expect(violations, `${label} Axe violations`).toEqual([]);
  const undersizedTargets = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(
      'button, input:not([type="hidden"]), select, textarea, summary, [role="button"]'
    )).flatMap((control) => {
      if (getComputedStyle(control).visibility === "hidden" || control.getClientRects().length === 0) return [];
      const input = control instanceof HTMLInputElement ? control : null;
      const target = input && (input.type === "checkbox" || input.type === "radio")
        ? input.closest<HTMLElement>("label") ?? input
        : control;
      const box = target.getBoundingClientRect();
      if (box.width + 0.5 >= 24 && box.height + 0.5 >= 24) return [];
      const name = control.getAttribute("aria-label") || control.textContent?.trim() || control.tagName;
      return [`${control.tagName.toLowerCase()} “${name.slice(0, 60)}” ${box.width.toFixed(1)}×${box.height.toFixed(1)}`];
    })
  );
  expect(undersizedTargets, `${label} undersized control targets`).toEqual([]);
}

const DRAWER_WIDTHS = [
  [760, "stacked"],
  [420, "single"],
  [330, "single"],
] as const;

for (const [width, layout] of DRAWER_WIDTHS) {
  test(`${width}px navigation drawer is complete, modal, and keyboard-contained`, async ({ page }) => {
    await installFixtures(page, { mutableWorkflows: true });
    await page.addInitScript(() => {
      localStorage.setItem("tare-recent-runs", JSON.stringify(["run-profile-fixture"]));
    });
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/index.html#/pulse");

    const shell = page.locator(".shell");
    const trigger = page.getByRole("button", { name: "Open navigation" });
    const nav = page.getByRole("navigation", { name: "Primary" });
    const close = page.getByRole("button", { name: "Close navigation" });
    const settings = nav.getByRole("link", { name: "Settings" });
    await expect(shell).toHaveAttribute("data-layout", layout);
    await expect(trigger).toBeVisible();

    await trigger.click();
    await expect(shell).toHaveClass(/nav-drawer-open/);
    await expect(trigger).toHaveAttribute("aria-expanded", "true");
    await expect(close).toBeFocused();
    for (const selector of [".brand", ".topbar", ".main", ".statusbar"]) {
      await expect(page.locator(selector)).toHaveAttribute("inert", "");
    }
    await expectAccessibleState(page, `${width}px open navigation drawer`);

    // Compact navigation exposes the same complete information architecture as the wide rail.
    await expect(nav.getByText("Matched nightly cohort", { exact: true })).toBeVisible();
    await expect(nav.getByTitle("run-profile-fixture")).toBeVisible();
    await expect(nav.getByText("Commands", { exact: true })).toBeVisible();
    for (const utility of ["Capture", "Trust & pricing", "Settings"]) {
      await expect(nav.getByRole("link", { name: utility })).toBeVisible();
    }

    // Shift+Tab from the first control wraps to the last; Tab from the last wraps to the first.
    await page.keyboard.press("Shift+Tab");
    await expect(settings).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(close).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(shell).not.toHaveClass(/nav-drawer-open/);
    await expect(trigger).toHaveAttribute("aria-expanded", "false");
    await expect(trigger).toBeFocused();

    // The backdrop is an independent, pointer-usable exit.
    await trigger.click();
    const backdrop = page.locator(".nav-drawer-backdrop");
    const backdropBox = await backdrop.boundingBox();
    expect(backdropBox).not.toBeNull();
    await page.mouse.click(backdropBox!.x + backdropBox!.width - 2, backdropBox!.y + 80);
    await expect(shell).not.toHaveClass(/nav-drawer-open/);
    await expect(trigger).toBeFocused();

    // Activating a destination closes the drawer and restores a stable navigation focus point.
    await trigger.click();
    await nav.getByRole("link", { name: "Investigate", exact: true }).click();
    await expect(page).toHaveURL(/#\/investigate/);
    await expect(shell).not.toHaveClass(/nav-drawer-open/);
    await expect(trigger).toBeFocused();
  });
}

test("a stale recent run self-heals with a coherent, equally-sized recovery choice", async ({ page }) => {
  const staleRun = "74565770-5475-453a-b21c-23390ba56fe7";
  await installFixtures(page, { mutableWorkflows: true });
  await page.route(`**/__tare/run_status?run=${staleRun}`, (route) => route.fulfill({
    status: 404,
    contentType: "application/json",
    body: JSON.stringify({ error: `run ${staleRun} not found` }),
  }));
  await page.addInitScript((runId) => {
    localStorage.setItem("tare-onboarded", "1");
    localStorage.setItem("tare-recent-runs", JSON.stringify([runId, "run-profile-fixture"]));
    localStorage.setItem("tare-pinned-runs", JSON.stringify([runId]));
    localStorage.setItem("tare-baseline-run", runId);
  }, staleRun);
  await page.setViewportSize({ width: 900, height: 700 });
  await page.goto(`/index.html#/investigate/run/${staleRun}`);

  await expect(page.locator(".error[role='alert']")).toContainText(
    "is no longer available in the active capture store"
  );
  await expect(page.locator(`.nav-runs [title="${staleRun}"]`)).toHaveCount(0);
  const actions = page.locator(".error-actions .btn");
  await expect(actions).toHaveCount(2);
  const [retryBox, backBox] = await Promise.all([actions.nth(0).boundingBox(), actions.nth(1).boundingBox()]);
  expect(retryBox).not.toBeNull();
  expect(backBox).not.toBeNull();
  expect(Math.abs(retryBox!.width - backBox!.width)).toBeLessThanOrEqual(0.5);
  expect(Math.abs(retryBox!.height - backBox!.height)).toBeLessThanOrEqual(0.5);
  await expectAccessibleState(page, "stale-run recovery");

  const prefs = await page.evaluate(() => ({
    recent: JSON.parse(localStorage.getItem("tare-recent-runs") ?? "[]") as string[],
    pinned: JSON.parse(localStorage.getItem("tare-pinned-runs") ?? "[]") as string[],
    baseline: localStorage.getItem("tare-baseline-run"),
  }));
  expect(prefs.recent).toEqual(["run-profile-fixture"]);
  expect(prefs.pinned).toEqual([]);
  expect(prefs.baseline).toBeNull();

  await page.getByRole("link", { name: "Back to runs" }).click();
  await expect(page).toHaveURL(/#\/investigate\?entity=runs/);
  await expect(page.locator(".investigate")).toBeVisible();
});

test("deep-link dates override persisted range and drive URL, UI, state, and API together", async ({ page }) => {
  const resolved: Array<Record<string, unknown>> = [];
  page.on("request", (request) => {
    if (!request.url().endsWith("/__tare/cohort/resolve")) return;
    try {
      resolved.push(request.postDataJSON() as Record<string, unknown>);
    } catch {
      // A malformed request is asserted below as a missing matching scope.
    }
  });
  await installFixtures(page);
  await page.addInitScript(() => {
    localStorage.setItem(
      "tare-range",
      JSON.stringify({ key: "custom", from: "2025-01-01", to: "2025-01-02" })
    );
  });
  await page.goto(
    "/index.html#/investigate?from=2026-07-02&to=2026-07-04&tz=UTC&range=custom"
  );

  await expect(page.getByLabel("Analysis time range")).toHaveValue("custom");
  await expect(page.getByLabel("From date")).toHaveValue("2026-07-02");
  await expect(page.getByLabel("To date")).toHaveValue("2026-07-04");
  await expect(page.locator('[data-pane="canvas"]')).toBeVisible();
  await expect.poll(() =>
    resolved.some(
      (scope) =>
        scope.from === "2026-07-02" && scope.to === "2026-07-04" && scope.timezone === "UTC"
    )
  ).toBe(true);
  const persisted = await page.evaluate(() => JSON.parse(localStorage.getItem("tare-range") ?? "{}"));
  expect(persisted).toEqual({ key: "custom", from: "2026-07-02", to: "2026-07-04" });
  const query = await page.evaluate(() =>
    Object.fromEntries(new URLSearchParams(location.hash.split("?")[1] ?? ""))
  );
  expect(query).toMatchObject({ from: "2026-07-02", to: "2026-07-04", tz: "UTC", range: "custom" });
});

test("invalid custom range is explained inline and can be corrected without losing context", async ({ page }) => {
  await installFixtures(page);
  await page.goto(
    "/index.html#/investigate?from=2026-07-02&to=2026-07-04&tz=UTC&range=custom"
  );
  const from = page.getByLabel("From date");
  const to = page.getByLabel("To date");
  const originalUrl = page.url();

  await from.fill("2026-07-10");
  await from.press("Tab");
  await expect(page.locator("#analysis-range-message")).toHaveText(
    "Choose a valid From and To date; From must not be after To."
  );
  await expect(from).toHaveAttribute("aria-invalid", "true");
  await expect(to).toHaveAttribute("aria-invalid", "true");
  expect(page.url()).toBe(originalUrl);
  await expectAccessibleState(page, "invalid custom range");

  await from.fill("2026-07-03");
  await from.press("Tab");
  await expect(page).toHaveURL(/from=2026-07-03/);
  await expect(from).not.toHaveAttribute("aria-invalid", "true");
  await expect(to).not.toHaveAttribute("aria-invalid", "true");
});

test("Explain is a visible control at medium widths and opens the supplementary pane", async ({ page }) => {
  await installFixtures(page);
  await page.setViewportSize({ width: 900, height: 800 });
  await page.goto("/index.html#/investigate");

  // Use the stable control identity: its accessible name intentionally changes while expanded.
  const trigger = page.locator("[data-pane-inspector]");
  const inspector = page.locator('[data-pane="inspector"]');
  await expect(trigger).toBeVisible();
  await expect(inspector).toBeHidden();
  await trigger.click();
  await expect(trigger).toHaveAttribute("aria-expanded", "true");
  await expect(trigger).toHaveText("Close explanation");
  await expect(inspector).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(trigger).toHaveAttribute("aria-expanded", "false");
  await expect(inspector).toBeHidden();
});

test("Optimize puts the actionable queue before collapsed secondary analytics", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/optimize");

  const queue = page.locator(".optimize-queue");
  const analytics = page.locator("details.optimize-analytics");
  await expect(queue).toContainText("Right-size the nightly classifier");
  await expect(analytics).not.toHaveAttribute("open", "");
  expect(
    await page.evaluate(() => {
      const actionQueue = document.querySelector(".optimize-queue");
      const secondary = document.querySelector(".optimize-analytics");
      return !!(
        actionQueue &&
        secondary &&
        actionQueue.compareDocumentPosition(secondary) & Node.DOCUMENT_POSITION_FOLLOWING
      );
    })
  ).toBe(true);
});

test("Pulse driver opens its representative Run Profile without dropping Selection A", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/pulse");
  await page.locator(".pulse-drv-row").click();

  await expect(page).toHaveURL(/#\/investigate\/run\/run-profile-fixture\?/);
  await expect(page).toHaveURL(/(?:\?|&)sel=/);
  await expect(page.locator(".run-profile")).toBeVisible();
});

test("budget Settings links can land directly on the Data section", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/index.html#/pulse?settings=data&sheet=settings");

  const nav = page.getByRole("navigation", { name: "Settings sections" });
  await expect(nav.getByRole("button", { name: "Data", exact: true })).toHaveAttribute(
    "aria-current",
    "location"
  );
  await expect(page.locator("#settings-section-data")).toBeInViewport();
});

test("phone Settings exposes and operates explicit section overflow controls", async ({ page }) => {
  await installFixtures(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/index.html#/pulse?sheet=settings");

  const nav = page.getByRole("navigation", { name: "Settings sections" });
  const later = page.getByRole("button", { name: "Show later settings sections" });
  await expect(nav).toBeVisible();
  await expect(later).toBeVisible();
  const start = await nav.evaluate((node) => node.scrollLeft);
  await later.click();
  await expect.poll(() => nav.evaluate((node) => node.scrollLeft)).toBeGreaterThan(start);
  await expect(nav.getByRole("button", { name: "Data", exact: true })).toBeInViewport();

  // Repeated forward navigation reaches the final section without a touch-only swipe gesture.
  for (let step = 0; step < 3 && await later.isVisible(); step += 1) {
    await later.click();
    await page.waitForTimeout(250);
  }
  const investigations = nav.getByRole("button", { name: "Investigations", exact: true });
  await expect(investigations).toBeInViewport();
  await investigations.click();
  await expect(investigations).toHaveAttribute("aria-current", "location");
  await expect(page.locator("#settings-section-saved-investigations")).toBeInViewport();
});

test("phone data tables explain horizontal overflow and update the cue direction", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/index.html#/investigate?mode=distinguish");

  const table = page.locator(".investigate-advanced .datatable").first();
  const cue = table.locator(".datatable-overflow-cue");
  await expect(table).toBeVisible();
  await expect(cue).toHaveText("Scroll for more columns →");
  expect(await table.evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);
  await table.evaluate((node) => {
    node.scrollLeft = node.scrollWidth;
    node.dispatchEvent(new Event("scroll"));
  });
  await expect(cue).toHaveText("← Scroll for earlier columns");
});

for (const width of [760, 420, 330]) {
  test(`Run Profile reflows without page-level horizontal clipping at ${width}px`, async ({ page }) => {
    await installFixtures(page);
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/index.html#/investigate/run/run-profile-fixture?view=profile");
    const profile = page.locator(".run-profile");
    await expect(profile).toBeVisible();

    const geometry = await page.evaluate(() => ({
      viewport: innerWidth,
      document: document.documentElement.scrollWidth,
      body: document.body.scrollWidth,
      shell: document.querySelector<HTMLElement>(".shell")?.scrollWidth ?? 0,
    }));
    expect(geometry.document).toBeLessThanOrEqual(geometry.viewport + 1);
    expect(geometry.body).toBeLessThanOrEqual(geometry.viewport + 1);
    expect(geometry.shell).toBeLessThanOrEqual(geometry.viewport + 1);
    const box = await profile.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(width + 1);
  });
}

test("palette exposes coherent groups plus saved-view and recent-run context", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.addInitScript(() => {
    localStorage.setItem("tare-recent-runs", JSON.stringify(["run-profile-fixture"]));
  });
  await page.setViewportSize({ width: 1440, height: 800 });
  await page.goto("/index.html#/pulse");
  await expect(page.locator(".nav-run-meta")).toHaveText(
    "claude-sonnet-4 · 2026-07-14"
  );
  await page.keyboard.press("Control+k");

  await expect(page.locator(".palette-group-label")).toHaveText([
    "Primary navigation",
    "Saved views",
    "Recent runs",
    "Actions",
    "Advanced",
  ]);
  const saved = page.locator(".palette-item", { hasText: "Matched nightly cohort" });
  await expect(saved.locator(".palette-item-subtitle")).toHaveText(
    "Optimize · 2026-07-01–2026-07-07 · 1 filter"
  );
  const recent = page.locator(".palette-item", { hasText: "run-profile-fixture" });
  await expect(recent.locator(".palette-item-subtitle")).toHaveText(
    "claude-sonnet-4 · 2026-07-14"
  );
  await expectAccessibleState(page, "open command palette");
  await page.getByRole("combobox", { name: "Command palette" }).fill("claude 2026");
  await expect(page.locator(".palette-item-title")).toHaveText("run-profile-fixture");
});

test("Pulse forecast stays interpretable at desktop and phone widths", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/pulse");

  const wide = page.locator(".pulse-chart-wide");
  await expect(wide).toBeVisible();
  await expect(page.locator(".pulse-chart-compact")).toBeHidden();
  await expect(wide).toHaveAttribute(
    "aria-label",
    "Interactive cumulative captured-spend projection chart. Use Left and Right arrow keys to inspect daily values."
  );
  await expect(wide).toHaveAttribute("aria-describedby", "pulse-forecast-desc-wide");
  await expect(wide.locator("desc#pulse-forecast-desc-wide")).toHaveText(
    /captured spend from .* through .* typical-pace period-end projection .* recent-pace scenarios run from .* not a probability interval.*configured cap/i
  );
  await expect(wide.locator(".pulse-gridline")).toHaveCount(5);
  await expect(wide.locator(".pulse-y-tick")).toContainText([
    "$100",
    "$75",
    "$50",
    "$25",
    "$0",
  ]);
  await expect(wide.locator(".pulse-band")).toHaveCount(1);
  await expect(wide.locator(".pulse-today-label")).toContainText("Today · Jul 14");
  const wideBox = await wide.boundingBox();
  expect(wideBox).not.toBeNull();
  expect(wideBox!.width).toBeGreaterThan(550);
  expect(wideBox!.height).toBeGreaterThan(200);

  await page.setViewportSize({ width: 390, height: 844 });
  const compact = page.locator(".pulse-chart-compact");
  await expect(compact).toBeVisible();
  await expect(wide).toBeHidden();
  const directLabels = compact.locator(".pulse-direct-label");
  await expect(directLabels).toHaveCount(5);
  await expect(directLabels).toContainText(["Captured", "Typical", "High", "Low", "Cap"]);
  const smallestLabelHeight = await directLabels.evaluateAll((nodes) =>
    Math.min(...nodes.map((node) => node.getBoundingClientRect().height))
  );
  expect(smallestLabelHeight).toBeGreaterThanOrEqual(9);
});

test("Investigate's persistent filter pane earns its space and avoids an identity comparison", async ({ page }) => {
  const resolved: Array<Record<string, unknown>> = [];
  page.on("request", (request) => {
    if (!request.url().endsWith("/__tare/cohort/resolve")) return;
    try {
      resolved.push(request.postDataJSON() as Record<string, unknown>);
    } catch {
      // The assertion below treats an unreadable request as a missing real facet selection.
    }
  });
  await installFixtures(page, { mutableWorkflows: true });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/index.html#/investigate");

  await expect(page.locator(".inv-result-facts")).toContainText("1,000 runs");
  await expect(page.locator(".inv-result-facts")).toContainText("Listed spend");
  await expect(page.locator(".inv-result-facts")).toContainText("<1%");
  await expect(page.locator('.inv-quick-facet[aria-label="Filter by model"]')).toContainText("claude-sonnet-4");
  await expect(page.locator('.inv-quick-facet[aria-label="Filter by provider"]')).toBeVisible();
  await expect(page.locator(".insp-start")).toContainText("Start with a selection");
  await expect(page.locator(".insp-decomp")).toHaveCount(0);

  const modeSizes = await page.locator(".inv-mode").evaluateAll((nodes) =>
    nodes.map((node) => {
      const box = node.getBoundingClientRect();
      return { x: box.x, y: box.y, width: box.width, height: box.height };
    })
  );
  expect(modeSizes).toHaveLength(5);
  for (const size of modeSizes.slice(1)) {
    expect(Math.abs(size.y - modeSizes[0].y)).toBeLessThanOrEqual(1);
    expect(Math.abs(size.height - modeSizes[0].height)).toBeLessThanOrEqual(1);
  }
  expect(modeSizes[2].width).toBeGreaterThan(modeSizes[0].width); // content-sized, not equal-width cards
  expect(modeSizes.every(
    (size, index) => index === 0 || size.x >= modeSizes[index - 1].x + modeSizes[index - 1].width - 1
  )).toBe(true);

  await page.locator('.inv-quick-facet[aria-label="Filter by model"] .inv-facet-row').first().click();
  await expect(page).toHaveURL(/(?:\?|&)sel=/);
  await expect(page.locator(".inv-selection")).toContainText("Selection A · 1 filter");
  await expect(page.locator('.inv-quick-facet[aria-label="Filter by model"] .inv-facet-row.active')).toBeVisible();
  await expect.poll(() => resolved.some((request) => {
    const filters = request.filters as Array<Record<string, unknown>> | undefined;
    return filters?.some((filter) =>
      filter.op === "eq" && filter.dimension === "model" && filter.value === "claude-sonnet-4"
    ) ?? false;
  })).toBe(true);
  await expect(page.locator(".insp-start")).toHaveCount(0);

  await page.locator('.inv-quick-facet[aria-label="Filter by model"] .inv-facet-clear').click();
  await expect(page).toHaveURL(/(?:\?|&)sel=none(?:&|$)/);
  await expect(page.locator(".inv-selection")).toContainText("Selection A · Whole scope");
  await expect(page.locator(".insp-start")).toContainText("Start with a selection");

  // Browser history must restore both states; an omitted/ambiguous clear used to resurrect the
  // highlighted facet while leaving the result request unfiltered.
  await page.goBack();
  await expect(page.locator('.inv-quick-facet[aria-label="Filter by model"] .inv-facet-row.active')).toBeVisible();
  await page.goForward();
  await expect(page.locator(".inv-selection")).toContainText("Selection A · Whole scope");
});

test("onboarding offers an enabled, clearly labelled sample before capture exists", async ({ page }) => {
  await installFixtures(page);
  await page.addInitScript(() => {
    localStorage.removeItem("tare-onboarded");
    localStorage.removeItem("tare-onboard-step");
  });
  await page.goto("/index.html#/onboarding");
  await page.getByRole("button", { name: "Next → See spend" }).click();

  const sample = page.getByRole("button", { name: "Explore sample Run Profile →" });
  await expect(sample).toBeVisible();
  await expect(sample).toBeEnabled();
  await expect(page.locator(".onboard-wait")).toContainText("No captured spend yet");
  await expectAccessibleState(page, "onboarding no-data state");
  await sample.click();
  await expect(page).toHaveURL(/#\/investigate\/run\/demo/);
  await expect(page.locator(".run-profile")).toBeVisible();
});

test("a failed canonical data load exposes a working Retry path", async ({ page }) => {
  await installFixtures(page);
  let attempts = 0;
  await page.route("**/__tare/cohort/resolve", async (route) => {
    attempts += 1;
    if (attempts === 1) {
      await route.fulfill({
        status: 503,
        contentType: "application/json",
        body: JSON.stringify({ error: "temporary fixture outage" }),
      });
      return;
    }
    await route.fallback();
  });
  await page.goto("/index.html#/investigate");

  const error = page.locator(".error-state");
  await expect(error).toContainText("Investigate results couldn't load for this scope");
  const retry = error.getByRole("button", { name: "Retry" });
  await expect(retry).toBeVisible();
  await expectAccessibleState(page, "canonical load error");
  await retry.click();
  await expect(error).toHaveCount(0);
  await expect(page.locator(".inv-results .datatable")).toBeVisible();
  expect(attempts).toBeGreaterThanOrEqual(2);
});

test("route loading has an announced accessible state", async ({ page }) => {
  await installFixtures(page);
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  await page.route("**/__tare/cohort/resolve", async (route) => {
    await gate;
    await route.fallback();
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/index.html#/investigate");

  const loading = page.locator('.main [role="status"][aria-busy="true"]').first();
  await expect(loading).toHaveAccessibleName("Loading content");
  await expectAccessibleState(page, "route loading");
  release();
  await expect(page.locator(".investigate")).toBeVisible();
});

test("unknown route gives an accessible way home", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/index.html#/no-such-surface");

  await expect(page.locator(".empty-state")).toContainText("Page not found");
  await expect(page.getByRole("link", { name: "Go to Pulse →" })).toBeVisible();
  await expectAccessibleState(page, "not-found state");
});

test("mutation feedback toast is announced and manually dismissible", async ({ page }) => {
  await installFixtures(page, { mutableWorkflows: true });
  await page.goto("/index.html#/optimize?view=open");
  await page.locator(".optimize-lifecycle-row").getByRole("button", { name: "Mark applied" }).click();

  const toast = page.locator(".toast").filter({ hasText: "Marked applied against the exact cohort" });
  await expect(toast).toHaveAttribute("role", "status");
  await expect(toast.getByRole("button", { name: "Dismiss" })).toBeVisible();
  await expectAccessibleState(page, "mutation toast");
  await toast.getByRole("button", { name: "Dismiss" }).click();
  await expect(toast).toHaveCount(0);
});

test("primary surfaces have named controls, unique ids, and coherent heading structure", async ({ page }) => {
  test.setTimeout(60_000);
  await installFixtures(page, { mutableWorkflows: true });
  await page.setViewportSize({ width: 1440, height: 900 });
  const surfaces = [
    ["#/pulse", ".pulse"],
    ["#/investigate", ".investigate"],
    ["#/investigate/run/run-profile-fixture?view=profile", ".run-profile"],
    ["#/investigate/compare?runs=run-a,run-b", ".compare-summary-matrix"],
    ["#/optimize", ".optimize-workspace"],
    ["#/optimize?view=scenarios", ".optimize-scenarios"],
    ["#/pulse?sheet=capture", '[role="dialog"]'],
    ["#/pulse?sheet=trust", '[role="dialog"]'],
    ["#/pulse?sheet=settings", '[role="dialog"]'],
    ["#/onboarding", ".onboard"],
  ] as const;

  for (const [hash, ready] of surfaces) {
    await test.step(hash, async () => {
      await page.goto(`/index.html${hash}`);
      await expect(page.locator(ready)).toBeVisible();
      if (hash.includes("sheet=")) {
        await expect(page.getByRole("dialog").getByRole("heading", { level: 1 })).toHaveCount(1);
      } else {
        await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);
      }
      const duplicateIds = await page.evaluate(() => {
        const ids = [...document.querySelectorAll<HTMLElement>("[id]")].map((node) => node.id);
        return ids.filter((id, index) => ids.indexOf(id) !== index);
      });
      expect(duplicateIds, `${hash} duplicate ids`).toEqual([]);
      const controls = page.locator(
        'button:visible, a[href]:visible, input:visible, select:visible, summary:visible, [role="button"]:visible'
      );
      for (let index = 0; index < (await controls.count()); index += 1) {
        await expect(controls.nth(index), `${hash} control ${index}`).toHaveAccessibleName(/\S/);
      }
    });
  }
});
