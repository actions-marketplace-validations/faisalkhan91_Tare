import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { renderSvg, renderSvgThemed, type FlamegraphModel } from "../src/svg.js";

// vitest runs with cwd = the web package dir.
function golden(name: string): string {
  return readFileSync(resolve(process.cwd(), "../tare-core/tests/golden", name), "utf8");
}

describe("shared SVG renderer", () => {
  it("produces byte-identical SVG to the Rust core for the committed model", () => {
    const model = JSON.parse(golden("flamegraph_bloated.json")) as FlamegraphModel;
    const svg = renderSvg(model);
    expect(svg).toBe(golden("flamegraph_bloated.svg"));
  });

  it("renders the no-usage placeholder for an empty model", () => {
    const empty: FlamegraphModel = {
      run_id: "x",
      pricing_version: "v",
      effective_date: "d",
      root: { name: "run x", tokens: 0, micros: 0, children: [] },
    };
    expect(renderSvg(empty)).toContain("No usage");
  });

  it("renderSvgThemed twins the export geometry but uses live CSS tokens", () => {
    const model = JSON.parse(golden("flamegraph_bloated.json")) as FlamegraphModel;
    const themed = renderSvgThemed(model);
    const exported = renderSvg(model);
    // Same geometry: identical width/height/viewBox (only colors differ).
    const dims = (s: string) => s.match(/width="\d+" height="\d+" viewBox="[^"]+"/)?.[0];
    expect(dims(themed)).toBe(dims(exported));
    // Themed colors from tokens, never the fixed export slab.
    expect(themed).toContain("var(--text)"); // labels
    expect(themed).toContain('fill="transparent"'); // background
    expect(themed).toContain("var(--cat-1)"); // fresh cache-class fill — categorical ink, not the brand accent (P3b)
    expect(themed).not.toContain("var(--accent)"); // the brand accent is never a data fill
    expect(themed).not.toContain("#ffffff");
    expect(themed).not.toContain("#1c1c1c");
    // The export path is unchanged — still the byte-golden slab.
    expect(exported).toContain("#ffffff");
  });
});
