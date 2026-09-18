// Page-by-page release coverage. This deliberately walks every user-facing canonical surface at a
// wide desktop and a narrow phone viewport. It complements feature journeys by enforcing the
// shared release contract on every page: the route resolves, loading settles, no visible error or
// page-level horizontal overflow remains, headings/controls are named, and the owning workspace is
// selected. Set TARE_PAGE_SCREENSHOT_DIR to retain viewport screenshots for a human visual pass.

import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { installFixtures } from "./fixtures.js";

interface PageSurface {
  name: string;
  hash: string;
  ready: string;
  workspace?: "pulse" | "investigate" | "optimize";
}

// Each matrix test performs 27 full navigations and Axe scans. Keep these four exhaustive passes
// serial within the otherwise parallel E2E suite so they do not starve short interaction journeys
// or overload the fixture server when the full release gate runs on a many-core machine.
test.describe.configure({ mode: "serial" });

const SURFACES: PageSurface[] = [
  { name: "Pulse", hash: "#/pulse", ready: ".pulse", workspace: "pulse" },

  { name: "Investigate · Runs", hash: "#/investigate?entity=runs", ready: ".investigate", workspace: "investigate" },
  { name: "Investigate · Sessions", hash: "#/investigate?entity=sessions", ready: ".investigate", workspace: "investigate" },
  { name: "Investigate · Templates", hash: "#/investigate?entity=templates", ready: ".investigate", workspace: "investigate" },
  { name: "Investigate · Steps", hash: "#/investigate?entity=steps", ready: ".investigate", workspace: "investigate" },
  { name: "Investigate · Time", hash: "#/investigate?mode=timeline&from=2026-07-01&to=2026-07-07&tz=UTC", ready: ".investigate", workspace: "investigate" },
  { name: "Investigate · Facets", hash: "#/investigate?view=facets", ready: ".investigate", workspace: "investigate" },
  { name: "Investigate · Correlations", hash: "#/investigate?mode=distinguish", ready: ".investigate-advanced", workspace: "investigate" },
  { name: "Investigate · Lineage", hash: "#/investigate?entity=templates&mode=lineage", ready: ".investigate-advanced", workspace: "investigate" },
  { name: "Investigate · Work units", hash: "#/investigate?view=units", ready: ".investigate-advanced", workspace: "investigate" },

  { name: "Run Profile · Timeline", hash: "#/investigate/run/run-profile-fixture?view=timeline", ready: ".run-profile", workspace: "investigate" },
  { name: "Run Profile · Profile", hash: "#/investigate/run/run-profile-fixture?view=profile", ready: ".run-profile", workspace: "investigate" },
  { name: "Run Profile · Shape", hash: "#/investigate/run/run-profile-fixture?view=shape", ready: ".run-profile", workspace: "investigate" },
  { name: "Run Profile · Provenance", hash: "#/investigate/run/run-profile-fixture?view=provenance", ready: ".run-profile", workspace: "investigate" },
  { name: "Compare runs", hash: "#/investigate/compare?runs=run-a,run-b", ready: ".compare-summary-matrix", workspace: "investigate" },

  { name: "Optimize · Open", hash: "#/optimize?view=open", ready: ".optimize-workspace", workspace: "optimize" },
  { name: "Optimize · Applied", hash: "#/optimize?view=applied", ready: ".optimize-workspace", workspace: "optimize" },
  { name: "Optimize · Verifying", hash: "#/optimize?view=verifying", ready: ".optimize-workspace", workspace: "optimize" },
  { name: "Optimize · Observed reduction", hash: "#/optimize?view=observed-reduction", ready: ".optimize-workspace", workspace: "optimize" },
  { name: "Optimize · Not observed", hash: "#/optimize?view=not-observed", ready: ".optimize-workspace", workspace: "optimize" },
  { name: "Optimize · Dismissed", hash: "#/optimize?view=dismissed", ready: ".optimize-workspace", workspace: "optimize" },
  { name: "Optimize · Scenarios", hash: "#/optimize?view=scenarios", ready: ".optimize-scenarios", workspace: "optimize" },

  { name: "Capture", hash: "#/pulse?sheet=capture", ready: '[role="dialog"]', workspace: "pulse" },
  { name: "Trust overview", hash: "#/pulse?sheet=trust", ready: '[role="dialog"]', workspace: "pulse" },
  { name: "Pricing catalog", hash: "#/pulse?sheet=trust&view=pricing", ready: '[role="dialog"]', workspace: "pulse" },
  { name: "Settings", hash: "#/pulse?sheet=settings", ready: '[role="dialog"]', workspace: "pulse" },
  { name: "Onboarding", hash: "#/onboarding", ready: ".onboard" },
];

const VIEWPORTS = [
  { name: "desktop-light", width: 1440, height: 900, colorScheme: "light" },
  { name: "intermediate-light", width: 900, height: 800, colorScheme: "light" },
  { name: "phone-light", width: 390, height: 844, colorScheme: "light" },
  { name: "desktop-dark", width: 1100, height: 800, colorScheme: "dark" },
] as const;

function slug(value: string): string {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

async function checkSurface(page: Page, surface: PageSurface, viewport: typeof VIEWPORTS[number]): Promise<void> {
  const pageErrors: string[] = [];
  const onPageError = (error: Error): void => { pageErrors.push(error.message); };
  page.on("pageerror", onPageError);
  await page.goto(`/index.html${surface.hash}`);
  await expect(page.locator(surface.ready).first(), `${surface.name} ready`).toBeVisible();
  await expect(page.locator(".main .skeleton:visible"), `${surface.name} loading settled`).toHaveCount(0);
  await expect(page.locator(".main .error-state:visible, .main .error:visible"), `${surface.name} visible errors`).toHaveCount(0);

  const geometry = await page.evaluate(() => ({
    viewport: innerWidth,
    document: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
    shell: document.querySelector<HTMLElement>(".shell")?.scrollWidth ?? 0,
  }));
  expect(geometry.document, `${surface.name} document overflow`).toBeLessThanOrEqual(geometry.viewport + 1);
  expect(geometry.body, `${surface.name} body overflow`).toBeLessThanOrEqual(geometry.viewport + 1);
  expect(geometry.shell, `${surface.name} shell overflow`).toBeLessThanOrEqual(geometry.viewport + 1);

  const chrome = await page.evaluate(() => {
    const topbar = document.querySelector<HTMLElement>(".topbar")?.getBoundingClientRect();
    const main = document.querySelector<HTMLElement>(".main")?.getBoundingClientRect();
    return topbar && main ? { topbarBottom: topbar.bottom, mainTop: main.top } : null;
  });
  if (chrome) {
    expect(chrome.topbarBottom, `${surface.name} topbar overlaps content`).toBeLessThanOrEqual(chrome.mainTop + 1);
  }

  const duplicateIds = await page.evaluate(() => {
    const ids = [...document.querySelectorAll<HTMLElement>("[id]")].map((node) => node.id);
    return [...new Set(ids.filter((id, index) => ids.indexOf(id) !== index))];
  });
  expect(duplicateIds, `${surface.name} duplicate ids`).toEqual([]);

  const dialog = surface.hash.includes("sheet=");
  if (dialog) {
    await expect(page.getByRole("dialog")).toHaveAttribute("aria-modal", "true");
    await expect(page.getByRole("dialog").getByRole("heading", { level: 1 })).toHaveCount(1);
    await expect(page.locator(".utility-sheet-close"), `${surface.name} initial dialog focus`).toBeFocused();
  } else {
    await expect(page.getByRole("heading", { level: 1 })).toHaveCount(1);
  }

  const controls = page.locator(
    'button:visible, a[href]:visible, input:visible, select:visible, textarea:visible, summary:visible, [role="button"]:visible'
  );
  for (let index = 0; index < await controls.count(); index += 1) {
    await expect(controls.nth(index), `${surface.name} control ${index}`).toHaveAccessibleName(/\S/);
  }

  // WCAG 2.2 SC 2.5.8: non-inline controls need at least a 24×24 CSS-pixel target. Native compact
  // checkboxes/radios use their enclosing label as the real pointer target.
  const undersizedTargets = await page.evaluate(() => {
    const controls = Array.from(document.querySelectorAll<HTMLElement>(
      'button, input:not([type="hidden"]), select, textarea, summary, [role="button"]'
    ));
    return controls.flatMap((control) => {
      if (getComputedStyle(control).visibility === "hidden" || control.getClientRects().length === 0) return [];
      const input = control instanceof HTMLInputElement ? control : null;
      const target = input && (input.type === "checkbox" || input.type === "radio")
        ? input.closest<HTMLElement>("label") ?? input
        : control;
      const rect = target.getBoundingClientRect();
      if (rect.width + 0.5 >= 24 && rect.height + 0.5 >= 24) return [];
      const name = control.getAttribute("aria-label") || control.textContent?.trim() || control.tagName;
      return [`${control.tagName.toLowerCase()} “${name.slice(0, 60)}” ${rect.width.toFixed(1)}×${rect.height.toFixed(1)}`];
    });
  });
  expect(undersizedTargets, `${surface.name} undersized control targets`).toEqual([]);

  if (surface.workspace) {
    await expect(
      page.locator(`.sidebar a[href^="#/${surface.workspace}"]`).first(),
      `${surface.name} owning workspace`
    ).toHaveAttribute("aria-current", "page");
  }
  if (surface.name === "Investigate · Facets") {
    await expect(page.locator('[data-pane="inspector"]'), `${surface.name} reveals inspector`).toBeVisible();
    if (viewport.width <= 520) {
      await expect(page.locator(".investigate"), `${surface.name} activates inspector level`).toHaveAttribute("data-active-pane", "inspector");
    }
  }
  if (
    viewport.width <= 520 &&
    surface.ready === ".investigate" &&
    surface.name !== "Investigate · Facets"
  ) {
    await expect(page.locator(".investigate"), `${surface.name} opens its result`).toHaveAttribute("data-active-pane", "canvas");
  }
  if (surface.name === "Optimize · Scenarios" && viewport.width <= 520) {
    const columnCount = await page.locator(".scenario-source-grid").evaluate((node) =>
      getComputedStyle(node).gridTemplateColumns.trim().split(/\s+/).filter(Boolean).length
    );
    expect(columnCount, `${surface.name} source fields stack`).toBe(1);
  }
  if (surface.name === "Settings") {
    await expect(page.getByRole("navigation", { name: "Settings sections" })).toBeVisible();
    await expect(page.getByRole("navigation", { name: "Settings sections" }).getByRole("button")).toHaveCount(7);
    if (viewport.width <= 520) {
      await expect(page.getByRole("button", { name: "Show later settings sections" })).toBeVisible();
    }
  }
  if (surface.name === "Investigate · Correlations" && viewport.width <= 520) {
    await expect(page.locator(".investigate-advanced .datatable-overflow-cue").first()).toContainText(
      "Scroll for more columns"
    );
  }
  if (surface.ready === ".investigate") {
    await expect(page.locator('[role="region"][aria-label="Investigation canvas"]')).toHaveCount(1);
    await expect(page.locator('[role="region"][aria-label="Inspector"]')).toHaveCount(1);
  }
  for (const link of await page.locator(".pulse-trust-details:visible, .insp-run:visible").all()) {
    const box = await link.boundingBox();
    expect(box?.height ?? 0, `${surface.name} compact link target height`).toBeGreaterThanOrEqual(24);
  }
  const axe = await new AxeBuilder({ page }).analyze();
  const axeViolations = axe.violations.map(({ id, impact, help, nodes }) => ({
    id,
    impact,
    help,
    // Keep failure output actionable and bounded even on a very large data surface.
    targets: nodes.slice(0, 5).map((node) => node.target.join(" ")),
  }));
  expect(axeViolations, `${surface.name} Axe violations`).toEqual([]);
  expect(pageErrors, `${surface.name} uncaught errors`).toEqual([]);
  page.off("pageerror", onPageError);

  const screenshotDir = process.env.TARE_PAGE_SCREENSHOT_DIR;
  if (screenshotDir) {
    await page.screenshot({
      path: `${screenshotDir}/${viewport.name}-${slug(surface.name)}.png`,
      animations: "disabled",
    });
  }
}

for (const viewport of VIEWPORTS) {
  test(`page-by-page ${viewport.name} coverage`, async ({ page }) => {
    test.setTimeout(180_000);
    await installFixtures(page, { mutableWorkflows: true });
    await page.emulateMedia({ colorScheme: viewport.colorScheme });
    await page.addInitScript(() => {
      localStorage.setItem("tare-onboarded", "true");
      localStorage.setItem("tare-theme", "system");
      localStorage.setItem("tare-recent-runs", JSON.stringify(["run-profile-fixture"]));
    });
    await page.setViewportSize({ width: viewport.width, height: viewport.height });
    for (const surface of SURFACES) {
      await test.step(surface.name, () => checkSurface(page, surface, viewport));
    }
  });
}
