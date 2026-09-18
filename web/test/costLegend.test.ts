import { describe, it, expect } from "vitest";
import { costBandLegend } from "../src/ui/costLegend.js";

describe("costBandLegend (shared cost-tone key)", () => {
  it("maps the three cost tones to cheaper→costlier with an accessible name", () => {
    const legend = costBandLegend("$ spend");
    // Three swatches, one per cost tone, in ascending-cost order.
    const swatches = Array.from(legend.querySelectorAll(".cost-legend-swatch"));
    expect(swatches.map((s) => s.className)).toEqual([
      "cost-legend-swatch cost-ok",
      "cost-legend-swatch cost-warn",
      "cost-legend-swatch cost-high",
    ]);
    // The direction is stated in text, not left to color perception alone.
    expect(legend.textContent).toContain("cheaper");
    expect(legend.textContent).toContain("costlier");
    // Accessible name names what the color encodes + the direction.
    expect(legend.getAttribute("role")).toBe("img");
    expect(legend.getAttribute("aria-label")).toBe("Color key by $ spend: cheaper, mid, then costlier");
  });
});
