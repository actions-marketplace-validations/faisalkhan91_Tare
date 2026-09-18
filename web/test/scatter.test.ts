import { describe, it, expect } from "vitest";
import { scatter } from "../src/ui/scatter.js";

describe("scatter (gridlines, ticks, and ring dots)", () => {
  const pts = [
    { x: 10, y: 100, label: "a" },
    { x: 50, y: 400, label: "b", tone: "cost-high" },
  ];

  it("draws median crosshairs only when quadrant is opted in", () => {
    // Off by default → existing callers stay byte-identical (no crosshair lines).
    expect(scatter(pts).querySelectorAll(".scatter-quadrant").length).toBe(0);
    // Opt in → a vertical + horizontal median line split the plot into four quadrants.
    expect(scatter(pts, { quadrant: true }).querySelectorAll(".scatter-quadrant").length).toBe(2);
  });

  it("labels the four quadrant regions when the caller names them", () => {
    const s = scatter(pts, {
      quadrant: true,
      quadrantLabels: { tr: "slow + costly", tl: "fast + costly", br: "slow + cheap", bl: "fast + cheap" },
    });
    const labels = Array.from(s.querySelectorAll(".scatter-quadrant-label")).map((t) => t.textContent);
    expect(labels).toEqual(["slow + costly", "fast + costly", "slow + cheap", "fast + cheap"]);
    // No labels rendered without the opt (and none when quadrant is off).
    expect(scatter(pts, { quadrant: true }).querySelectorAll(".scatter-quadrant-label").length).toBe(0);
  });

  it("draws two faint Y gridlines (max + midpoint) with tick values", () => {
    const s = scatter(pts);
    expect(s.querySelectorAll(".scatter-grid").length).toBe(2);
    const ticks = Array.from(s.querySelectorAll(".scatter-tick")).map((t) => t.textContent);
    expect(ticks).toContain("400"); // Y max
    expect(ticks).toContain("200"); // Y midpoint
    expect(ticks).toContain("50"); // X max
  });

  it("formats ticks compactly, or via a caller-supplied unit formatter", () => {
    const big = scatter([{ x: 1_200_000, y: 3_400_000, label: "x" }]);
    const t = Array.from(big.querySelectorAll(".scatter-tick")).map((x) => x.textContent);
    expect(t).toContain("3.4M");
    expect(t).toContain("1.2M");
    const usd = scatter([{ x: 5, y: 2_500_000, label: "y" }], {
      formatY: (n) => `$${(n / 1e6).toFixed(2)}`,
    });
    expect(Array.from(usd.querySelectorAll(".scatter-tick")).map((x) => x.textContent)).toContain("$2.50");
  });

  it("dots are rings carrying tone + optional size (weight 0..1 → r 3..6)", () => {
    const s = scatter([
      { x: 1, y: 1, label: "small", weight: 0 },
      { x: 2, y: 2, label: "big", weight: 1, tone: "cost-high" },
    ]);
    const dots = Array.from(s.querySelectorAll(".scatter-dot")) as SVGElement[];
    expect(dots.length).toBe(2);
    expect(dots[0].getAttribute("r")).toBe("3");
    expect(dots[1].getAttribute("r")).toBe("6");
    expect(dots[1].classList.contains("cost-high")).toBe(true);
    // Absent weight keeps the historical default radius.
    expect((scatter([{ x: 1, y: 1, label: "d" }]).querySelector(".scatter-dot"))!.getAttribute("r")).toBe("4");
  });

  it("renders the y-axis label inside the canvas (positive x, not clipped off-edge)", () => {
    const s = scatter([{ x: 1, y: 1, label: "p" }], { yLabel: "spend ↑" });
    const label = Array.from(s.querySelectorAll(".scatter-axis-label")).find(
      (t) => t.textContent === "spend ↑"
    ) as SVGElement | undefined;
    expect(label).toBeTruthy();
    const x = Number(label!.getAttribute("transform")!.match(/translate\((-?\d+)/)![1]);
    expect(x).toBeGreaterThan(0);
  });

  it("is byte-deterministic (same input → identical SVG)", () => {
    expect(scatter(pts).outerHTML).toBe(scatter(pts).outerHTML);
  });
});
