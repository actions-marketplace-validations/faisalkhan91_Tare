import { describe, it, expect } from "vitest";
import { scatter } from "../src/ui/scatter.js";
import { parcoords } from "../src/ui/parcoords.js";

// empty charts render a centered "No data" label, not a bare axis frame.
describe("chart empty states", () => {
  it("scatter with no points shows a centered No-data label", () => {
    const svg = scatter([]);
    const label = svg.querySelector(".chart-empty");
    expect(label?.textContent).toBe("No data");
    // No dots drawn.
    expect(svg.querySelectorAll(".scatter-dot").length).toBe(0);
  });

  it("scatter with points draws dots and no empty label", () => {
    const svg = scatter([{ x: 1, y: 2, label: "a" }]);
    expect(svg.querySelector(".chart-empty")).toBeNull();
    expect(svg.querySelectorAll(".scatter-dot").length).toBe(1);
  });

  it("parcoords with no rows shows a centered No-data label", () => {
    const svg = parcoords(["x", "y"], []);
    expect(svg.querySelector(".chart-empty")?.textContent).toBe("No data");
    expect(svg.querySelectorAll(".pc-line").length).toBe(0);
  });
});
