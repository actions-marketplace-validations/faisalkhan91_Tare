import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  renderTrendSvg,
  renderTrendSvgThemed,
  type TrendReport,
} from "../src/trendSvg.js";

// vitest runs with cwd = the web package dir.
function golden(name: string): string {
  return readFileSync(resolve(process.cwd(), "../tare-core/tests/golden", name), "utf8");
}

describe("shared trend SVG renderer", () => {
  it("produces byte-identical SVG to the Rust core for the committed report", () => {
    const report = JSON.parse(golden("trend.json")) as TrendReport;
    expect(renderTrendSvg(report)).toBe(golden("trend.svg"));
  });

  it("stays byte-identical to Rust for NON-ASCII series keys (legend scalar-count, #3)", () => {
    const report = JSON.parse(golden("trend_nonascii.json")) as TrendReport;
    expect(renderTrendSvg(report)).toBe(golden("trend_nonascii.svg"));
  });

  it("renders the no-usage placeholder for an empty / all-zero report", () => {
    const empty: TrendReport = {
      dimension: "total",
      from: "2026-06-24",
      to: "2026-06-24",
      days: ["2026-06-24"],
      series: [{ key: "total", per_day: [0], total_micros: 0 }],
      pricing_version: "v",
      estimated: true,
    };
    expect(renderTrendSvg(empty)).toContain("No usage");

    const noDays: TrendReport = {
      dimension: "total",
      from: "2026-06-25",
      to: "2026-06-24",
      days: [],
      series: [],
      pricing_version: "v",
      estimated: true,
    };
    expect(renderTrendSvg(noDays)).toContain("No usage");
  });
});

describe("renderTrendSvgThemed (on-screen)", () => {
  const report: TrendReport = {
    dimension: "by_model",
    from: "2026-06-23",
    to: "2026-06-24",
    days: ["2026-06-23", "2026-06-24"],
    series: [
      { key: "opus", per_day: [100, 200], total_micros: 300 },
      { key: "haiku", per_day: [50, 50], total_micros: 100 },
    ],
    pricing_version: "v",
    estimated: true,
  };

  it("colors from live CSS tokens, never the export's fixed hex palette", () => {
    const svg = renderTrendSvgThemed(report);
    // transparent bg + themed axes/series so the chart lives in the app surface.
    expect(svg).toContain('fill="transparent"');
    expect(svg).toContain("var(--cat-1)"); // first series — categorical ink (P3b), not accent/severity
    expect(svg).toContain("var(--cat-2)"); // second series
    expect(svg).not.toContain("var(--accent)"); // the brand accent is never a data series
    expect(svg).toContain("var(--muted)"); // axis dates
    expect(svg).toContain("var(--text)"); // legend labels
    // None of the export path's hardcoded colors leak into the themed variant.
    expect(svg).not.toContain("#ffffff");
    expect(svg).not.toContain("#1c1c1c");
    expect(svg).not.toContain("#4e79a7"); // Tableau palette[0]
  });

  it("preserves the export geometry exactly (only colors differ)", () => {
    const themed = renderTrendSvgThemed(report);
    const exported = renderTrendSvg(report);
    const bars = (s: string) => (s.match(/<g><rect /g) ?? []).length;
    // Same stacked-bar count and same canvas box — geometry is shared, not re-derived.
    expect(bars(themed)).toBe(bars(exported));
    const box = /width="\d+" height="\d+" viewBox="[^"]+"/;
    expect(themed.match(box)?.[0]).toBe(exported.match(box)?.[0]);
  });

  it("carries an on-axis dollar scale + estimated provenance on screen, never in the export", () => {
    const report = JSON.parse(golden("trend.json")) as TrendReport;
    const themed = renderTrendSvgThemed(report);
    // The peak stacked cost/day is shown as a dollar label + a labeled $0 baseline, not hover-only.
    expect(themed).toContain("peak/day");
    expect(themed).toContain("$0 baseline");
    expect(themed).toContain("Estimated. Pricing ");
    // A non-estimated report says "actual", not "estimated".
    expect(renderTrendSvgThemed({ ...report, estimated: false })).toContain("Actual. Pricing ");
    // The export path stays byte-clean: none of the screen-only scale text leaks in.
    const exported = renderTrendSvg(report);
    expect(exported).not.toContain("peak/day");
    expect(exported).not.toContain("baseline");
    expect(exported).not.toContain(". Pricing ");
  });

  it("keeps the no-usage placeholder legible on a dark surface", () => {
    const empty: TrendReport = {
      dimension: "total",
      from: "2026-06-24",
      to: "2026-06-24",
      days: ["2026-06-24"],
      series: [{ key: "total", per_day: [0], total_micros: 0 }],
      pricing_version: "v",
      estimated: true,
    };
    const svg = renderTrendSvgThemed(empty);
    expect(svg).toContain("No usage");
    expect(svg).toContain('fill="var(--muted)"'); // export omits this; screen adds it
  });
});
