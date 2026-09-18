// on-screen flamegraph frame width is honest — Cost (micros) by default, Tokens on
// toggle — and the export renderer stays token-weighted and byte-frozen. Cache-read/output tokens
// price differently, so a fixture with unequal unit prices must yield DIFFERENT widths per mode.
import { describe, it, expect } from "vitest";
import { renderSvg, renderSvgThemed, type FlamegraphModel } from "../src/svg.js";

// Two sibling leaves with EQUAL tokens but UNEQUAL cost (A is cheap, B is 9× dearer per token) —
// the classic cache-read-vs-output price gap. Cost mode must widen B; token mode splits them evenly.
const UNEQUAL_PRICES: FlamegraphModel = {
  run_id: "r",
  pricing_version: "v",
  effective_date: "d",
  root: {
    name: "run r",
    tokens: 200,
    micros: 1000,
    children: [
      { name: "A", tokens: 100, micros: 100, children: [] },
      { name: "B", tokens: 100, micros: 900, children: [] },
    ],
  },
};

// Frame <rect> widths, in document order (root first, then children), from a rendered flame SVG.
// Frame rects are height="28" (ROW_H-2); the 12px legend swatches are excluded.
function frameRectWidths(svg: string): number[] {
  return [...svg.matchAll(/<rect x="\d+" y="\d+" width="(\d+)" height="28"/g)].map((m) => Number(m[1]));
}
// The two child frames A, B (index 0 is the full-width root).
function childWidths(svg: string): [number, number] {
  const w = frameRectWidths(svg);
  return [w[1], w[2]];
}

describe("flame weighting", () => {
  it("cost and token modes give deterministic but different frame widths on unequal prices", () => {
    const cost = renderSvgThemed(UNEQUAL_PRICES, "cost");
    const tokens = renderSvgThemed(UNEQUAL_PRICES, "tokens");

    expect(cost).not.toBe(tokens); // different geometry
    expect(renderSvgThemed(UNEQUAL_PRICES, "cost")).toBe(cost); // deterministic
    expect(renderSvgThemed(UNEQUAL_PRICES, "tokens")).toBe(tokens);

    const [ca, cb] = childWidths(cost);
    const [ta, tb] = childWidths(tokens);
    // Tokens: equal split (100/100). Cost: B (900) far wider than A (100).
    expect(ta).toBe(tb);
    expect(cb).toBeGreaterThan(ca);
    expect(cb).toBeGreaterThan(tb); // cost concentrates width onto the dear frame
  });

  it("defaults to Cost weighting", () => {
    expect(renderSvgThemed(UNEQUAL_PRICES)).toBe(renderSvgThemed(UNEQUAL_PRICES, "cost"));
  });

  it("export renderer is always token-weighted (geometry matches themed Tokens, never themed Cost)", () => {
    const exportWidths = childWidths(renderSvg(UNEQUAL_PRICES));
    expect(exportWidths).toEqual(childWidths(renderSvgThemed(UNEQUAL_PRICES, "tokens")));
    expect(exportWidths[0]).toBe(exportWidths[1]); // equal tokens → equal export widths
  });

  it("cost mode over a fully-unpriced run says so instead of a blank or fake-$0 flame", () => {
    const unpriced: FlamegraphModel = {
      run_id: "u",
      pricing_version: "v",
      effective_date: "d",
      root: {
        name: "run u",
        tokens: 500,
        micros: 0,
        children: [{ name: "local", tokens: 500, micros: 0, children: [] }],
      },
    };
    expect(renderSvgThemed(unpriced, "cost")).toContain("unpriced");
    // Tokens mode still shows the work; export (token-weighted) also renders normally.
    expect(renderSvgThemed(unpriced, "tokens")).not.toContain("unpriced");
    expect(renderSvg(unpriced)).not.toContain("unpriced");
  });
});
