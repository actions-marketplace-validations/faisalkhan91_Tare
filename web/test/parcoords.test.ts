import { describe, it, expect } from "vitest";
import { parcoords } from "../src/ui/parcoords.js";

describe("parcoords per-axis numeric ticks", () => {
  const rows = [
    { label: "a", values: [100, 5_000_000], tone: "cost-ok" },
    { label: "b", values: [900, 1_000_000], tone: "cost-high" },
  ];

  it("renders a caller-formatted min + max tick per axis in that axis's unit", () => {
    const svg = parcoords(["tokens", "spend"], rows, {
      axisFormat: (i, v) => (i === 0 ? `${v} tok` : `$${(v / 1_000_000).toFixed(2)}`),
    });
    const ticks = Array.from(svg.querySelectorAll(".pc-axis-tick")).map((t) => t.textContent);
    // Axis 0 (tokens): max 900, min 100. Axis 1 (spend): max $5.00, min $1.00.
    expect(ticks).toContain("900 tok");
    expect(ticks).toContain("100 tok");
    expect(ticks).toContain("$5.00");
    expect(ticks).toContain("$1.00");
  });

  it("suppresses ticks on an axis whose formatter returns '' (binary/rank knobs)", () => {
    const svg = parcoords(["knob", "spend"], rows, {
      axisFormat: (i, v) => (i === 0 ? "" : `$${(v / 1_000_000).toFixed(2)}`),
    });
    const ticks = Array.from(svg.querySelectorAll(".pc-axis-tick")).map((t) => t.textContent);
    // Only the spend axis contributes ticks; the suppressed knob axis adds none.
    expect(ticks).toEqual(["$5.00", "$1.00"]);
  });

  it("renders no ticks without an axis formatter", () => {
    const svg = parcoords(["tokens", "spend"], rows, {});
    expect(svg.querySelectorAll(".pc-axis-tick").length).toBe(0);
  });
});
