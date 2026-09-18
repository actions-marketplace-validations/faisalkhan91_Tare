// Forced-color guards. Windows forced-colors / high-contrast erases the
// custom palette and hands color to the OS, so meaning must survive WITHOUT our colors. This locks the
// four important surface families — data / accent / warning / Beam — so a future change that
// starts relying on color alone fails CI. Complements the numeric contrast verifier (verify-contrast.py):
// contrast covers the normal palette, these guards cover the forced palette.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { scatter } from "../src/ui/scatter.js";
import { parcoords } from "../src/ui/parcoords.js";
import { tareBeam } from "../src/ui/tareBeam.js";

const WEB = process.cwd();
const app = readFileSync(resolve(WEB, "src/ui/app.css"), "utf8");
const tokens = readFileSync(resolve(WEB, "src/ui/tokens.css"), "utf8");
const components = readFileSync(resolve(WEB, "src/ui/components.css"), "utf8");

/// The body of the first `@media (forced-colors: active)` (optionally also prefers-contrast) block.
function forcedColorsBlocks(css: string): string {
  const out: string[] = [];
  const re = /@media[^{]*forced-colors:\s*active[^{]*\{/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(css)) !== null) {
    // Balance braces from the block opener to capture the whole at-rule body.
    let depth = 1;
    let i = m.index + m[0].length;
    for (; i < css.length && depth > 0; i++) {
      if (css[i] === "{") depth++;
      else if (css[i] === "}") depth--;
    }
    out.push(css.slice(m.index, i));
  }
  return out.join("\n");
}

describe("forced-color guards", () => {
  it("accent: focus + selection hand off to OS system colors under forced-colors", () => {
    const fc = forcedColorsBlocks(tokens);
    expect(fc).toMatch(/:focus-visible/);
    expect(fc).toMatch(/Highlight/); // outline → Highlight
    expect(fc).toMatch(/::selection[^}]*Highlight/s); // selection → Highlight / HighlightText
  });

  it("warning: severity is conveyed by a labelled currentColor icon, never color alone", () => {
    // The warning icon draws in currentColor (so forced-colors remaps it to system text) and carries an
    // accessible name — the meaning is the glyph + label, not the hue.
    const icon = readFileSync(resolve(WEB, "src/ui/icon.ts"), "utf8");
    expect(icon).toMatch(/warning:/); // registered glyph
    expect(icon).toMatch(/stroke",\s*"currentColor"|setAttribute\("stroke", "currentColor"\)/);
    // Icons opt into the OS palette under forced-colors.
    expect(forcedColorsBlocks(app)).toMatch(/\.icon[^}]*forced-color-adjust:\s*auto/s);
  });

  it("Beam: segments drop pattern fills for structural CanvasText borders under forced-colors", () => {
    const fc = forcedColorsBlocks(components);
    expect(fc).toMatch(/\.tare-beam-seg/);
    expect(fc).toMatch(/background-image:\s*none/);
    expect(fc).toMatch(/CanvasText/);
    // Sanity: the live Beam still renders its outline (the accessible source of truth) regardless.
    const b = tareBeam({
      mode: "run",
      title: "T",
      unit: "tokens",
      segments: [{ key: "a", label: "A", value: 2 }, { key: "b", label: "B", value: 1 }],
    });
    expect(b.querySelector(".tare-beam-outline")!.textContent).toContain("A");
  });

  it("data: categorical/data marks pair color with a non-color channel (title/label)", () => {
    // Forced-colors collapses categorical hues to system colors, so every data mark must also carry a
    // label. scatter tags each dot with a <title>; parcoords titles each line.
    const sc = scatter(
      [{ x: 1, y: 2, label: "alpha", tone: "cost-ok" }, { x: 3, y: 4, label: "beta", tone: "cost-high" }],
      { xLabel: "x", yLabel: "y" }
    );
    const dotTitles = Array.from(sc.querySelectorAll(".scatter-dot title")).map((t) => t.textContent);
    expect(dotTitles).toContain("alpha");
    expect(dotTitles).toContain("beta");

    const pc = parcoords(["a", "b"], [{ label: "row1", values: [1, 2], tone: "cost-ok" }]);
    expect(pc.querySelector(".pc-line title")?.textContent).toBe("row1");
  });
});
