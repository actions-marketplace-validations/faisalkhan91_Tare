// Guard: data is set in INK, never the brand accent. A category/series/bar drawn in
// the accent is exactly the "scarce accent is a lie" regression round-2/round-3/Codex all flagged, so we
// pin it with a source scan rather than trusting review. Screen SVG twins are checked too; the byte-golden
// EXPORT theme is deliberately NOT scanned (it is frozen against the Rust exporter).
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// vitest runs from web/ (see repo testing conventions), so src/ is under the cwd.
const read = (rel: string): string => readFileSync(resolve(process.cwd(), "src", rel), "utf8");

/// Extract the declaration block for an exact selector line (`selector {\n ... \n}`), so we can assert
/// what a specific rule paints without matching unrelated rules that merely share a class prefix.
function block(css: string, selector: string): string {
  const start = css.indexOf(selector + " {");
  if (start < 0) throw new Error(`selector not found: ${selector}`);
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  return css.slice(open + 1, close);
}

describe("data marks are ink, never the brand accent", () => {
  const appCss = read("ui/app.css");

  // Single-series magnitude marks → --data-ink.
  const singleSeries = [
    ".driver-bar",
    ".scatter-dot",
    ".pc-line",
  ];
  for (const sel of singleSeries) {
    it(`${sel} paints in ink, not --accent`, () => {
      const b = block(appCss, sel);
      expect(b).not.toContain("var(--accent)");
      expect(b).toContain("var(--data-ink)");
    });
  }

  it("the screen SVG twins (flame classes, trend series) carry no brand accent", () => {
    const svg = read("svg.ts");
    const trend = read("trendSvg.ts");
    // Only inspect the on-screen THEMED palettes; the byte-golden EXPORT_THEME is intentionally frozen.
    const themedClass = svg.slice(svg.indexOf("THEMED_CLASS_COLOR"), svg.indexOf("LEGEND_ORDER"));
    const themedPalette = trend.slice(trend.indexOf("THEMED_PALETTE"), trend.indexOf("]", trend.indexOf("THEMED_PALETTE")));
    expect(themedClass).not.toContain("var(--accent)");
    expect(themedClass).toContain("var(--cat-1)");
    expect(themedPalette).not.toContain("var(--accent)");
    expect(themedPalette).toContain("var(--cat-1)");
  });
});
