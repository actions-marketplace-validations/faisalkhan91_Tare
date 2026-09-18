import { expect, test } from "@playwright/test";
import { installFixtures } from "./fixtures.js";

const NO_CAP_BURNRATE = {
  run_rate_micros_per_day: 82_000_000,
  effective_rate_micros_per_day: 76_388_889,
  spent_micros: 937_000_000,
  active_days: 8,
  daily_spend_micros: [
    0, 0, 0, 0, 0, 330_000_000, 70_000_000, 0, 20_000_000,
    0, 390_000_000, 0, 0, 10_000_000, 80_000_000, 20_000_000, 17_000_000, 0,
  ],
  days_elapsed: 18,
  days_in_period: 30,
  projected_micros: 1_375_000_000,
  projected_low_micros: 1_044_000_000,
  projected_high_micros: 2_545_000_000,
  cap_micros: 0,
  on_track: true,
  headroom_days: null,
  period: "month",
  period_start: "2026-09-01",
  as_of: "2026-09-18",
  period_end: "2026-09-30",
};

test("Pulse no-cap forecast stays balanced from phone through ultrawide", async ({ page }) => {
  await installFixtures(page);
  // Register exact-case overrides after the broad fixture handler; Playwright evaluates routes in
  // reverse registration order. This mirrors the high-value/no-cap/OTel state that exposed the old
  // floating controls and unbounded SVG scaling.
  await page.route("**/__tare/burnrate*", (route) => route.fulfill({ json: NO_CAP_BURNRATE }));
  await page.route("**/__tare/coverage*", (route) => route.fulfill({
    json: {
      status: "amber",
      has_proxy: false,
      has_otel: true,
      blind_sources: [],
      sources: [{ source: "otel-event", steps: 8, last_day: "2026-09-18", heartbeat: true }],
    },
  }));
  await page.emulateMedia({ colorScheme: "dark" });
  await page.addInitScript(() => {
    localStorage.setItem("tare-onboarded", "true");
    localStorage.setItem("tare-theme", "system");
  });

  const cases = [
    { name: "ultrawide", width: 2048, height: 1200, chart: ".pulse-chart-ultrawide" },
    { name: "intermediate", width: 900, height: 900, chart: ".pulse-chart-medium" },
    { name: "phone", width: 390, height: 844, chart: ".pulse-chart-compact" },
  ];
  for (const layout of cases) {
    await test.step(layout.name, async () => {
      await page.setViewportSize({ width: layout.width, height: layout.height });
      await page.goto("/index.html#/pulse");
      await expect(page.locator(".pulse")).toBeVisible();
      await expect(page.locator(layout.chart)).toBeVisible();

      const geometry = await page.evaluate(() => {
        const rect = (selector: string): DOMRect =>
          document.querySelector<HTMLElement>(selector)!.getBoundingClientRect();
        const pulse = rect(".pulse");
        const chart = rect(".pulse-forecast-line");
        const chartFooter = rect(".pulse-chart-footer");
        const supportStrip = rect(".pulse-support");
        const scope = rect(".pulse-scope");
        const range = rect(".pulse-range-selector");
        const support = [...document.querySelectorAll<HTMLElement>(".pulse-support-item")]
          .map((node) => node.getBoundingClientRect().width);
        return {
          documentWidth: document.documentElement.scrollWidth,
          viewportWidth: innerWidth,
          pulseWidth: pulse.width,
          chartWidth: chart.width,
          chartHeight: chart.height,
          footerInsetStart: chartFooter.left - pulse.left,
          footerInsetEnd: pulse.right - chartFooter.right,
          supportInsetStart: supportStrip.left - pulse.left,
          supportInsetEnd: pulse.right - supportStrip.right,
          rangeGap: Math.max(0, range.left - scope.right),
          support,
        };
      });
      expect(geometry.documentWidth).toBeLessThanOrEqual(geometry.viewportWidth + 1);
      expect(geometry.chartWidth).toBeGreaterThanOrEqual(geometry.pulseWidth - 1);
      expect(Math.abs(geometry.footerInsetStart)).toBeLessThan(1);
      expect(Math.abs(geometry.footerInsetEnd)).toBeLessThan(1);
      expect(Math.abs(geometry.supportInsetStart)).toBeLessThan(1);
      expect(Math.abs(geometry.supportInsetEnd)).toBeLessThan(1);
      expect(Math.max(...geometry.support) - Math.min(...geometry.support)).toBeLessThan(1);
      if (layout.name === "ultrawide") expect(geometry.chartHeight).toBeLessThan(460);
      await expect(page.locator('.pulse-range-option[aria-current="true"]')).toHaveText("MTD");

      const interactiveChart = page.locator(layout.chart);
      const point = await interactiveChart.evaluate((svg, day) => {
        const rect = svg.getBoundingClientRect();
        const width = Number(svg.dataset.chartWidth);
        const left = Number(svg.dataset.plotLeft);
        const right = Number(svg.dataset.plotRight);
        const x = left + (Number(day) / Number(svg.dataset.days)) * (right - left);
        return { x: rect.left + x / width * rect.width, y: rect.top + rect.height * 0.45 };
      }, 10);
      await page.mouse.move(point.x, point.y);
      await expect(page.locator(".pulse-chart-tooltip")).toBeVisible();
      await expect(page.locator(".pulse-chart-tooltip")).toContainText("Captured cumulative");

      await interactiveChart.focus();
      await interactiveChart.press("End");
      await expect(page.locator(".pulse-chart-tooltip")).toContainText("Sep 30, 2026");
      await expect(page.locator(".pulse-chart-tooltip")).toContainText("Typical pace");
      await interactiveChart.press("Escape");
      await expect(page.locator(".pulse-chart-tooltip")).toBeHidden();

      const toggle = page.locator(".pulse-chart-data-toggle");
      await expect(toggle).toHaveAttribute("aria-expanded", "false");
      await toggle.click();
      await expect(toggle).toHaveAttribute("aria-expanded", "true");
      await expect(page.locator(".pulse-chart-data-panel")).toBeVisible();

      if (layout.name === "phone") {
        const labelGeometry = await page.locator(".pulse-chart-compact").evaluate((svg) => {
          const plotRight = Number(svg.querySelector(".pulse-gridline")?.getAttribute("x2"));
          const labels = [...svg.querySelectorAll<SVGTextElement>(".pulse-direct-label[class*='pulse-label-']")];
          return { plotRight, lefts: labels.map((label) => label.getBBox().x) };
        });
        expect(labelGeometry.lefts.every((left) => left > labelGeometry.plotRight)).toBe(true);
      }

      await toggle.click();
      await expect(toggle).toHaveAttribute("aria-expanded", "false");

      const screenshotDir = process.env.TARE_PULSE_SCREENSHOT_DIR;
      if (screenshotDir) {
        await page.screenshot({
          path: `${screenshotDir}/pulse-${layout.name}.png`,
          fullPage: true,
          animations: "disabled",
        });
      }
    });
  }
});

test("Pulse range presets query real data and survive reload", async ({ page }) => {
  await installFixtures(page);
  const requested: Array<string | null> = [];
  await page.route("**/__tare/burnrate*", (route) => {
    const range = new URL(route.request().url()).searchParams.get("range");
    requested.push(range);
    const year = range === "year";
    return route.fulfill({
      json: {
        ...NO_CAP_BURNRATE,
        period: year ? "year" : "month",
        period_start: year ? "2026-01-01" : "2026-09-01",
        period_end: year ? "2026-12-31" : "2026-09-30",
        as_of: year ? "2026-01-18" : "2026-09-18",
        days_in_period: year ? 365 : 30,
      },
    });
  });
  await page.addInitScript(() => localStorage.setItem("tare-onboarded", "true"));
  await page.goto("/index.html#/pulse");
  await expect(page.locator('.pulse-range-option[aria-current="true"]')).toHaveText("MTD");

  await page.locator('.pulse-range-option[aria-label="Show year to date spend"]').click();
  await expect(page).toHaveURL(/#\/pulse\?range=year$/);
  await expect(page.locator('.pulse-range-option[aria-current="true"]')).toHaveText("YTD");
  await expect(page.locator(".pulse-scope")).toContainText("Year to date");
  expect(requested).toContain("year");

  await page.reload();
  await expect(page.locator('.pulse-range-option[aria-current="true"]')).toHaveText("YTD");
  await expect(page.locator(".pulse-answer-label")).toContainText("this year");
});
