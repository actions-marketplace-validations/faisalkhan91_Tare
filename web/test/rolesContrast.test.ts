// Role-pair contrast: both themes pass all role pairs. Parses the
// committed OKLCH role tokens from tokens.css (bench identity, dark + light) and asserts each
// foreground role clears its WCAG bar against the surfaces it renders on, using the same
// luminance/contrast math the Python verifier cross-checks (accentContrast.ts). This is the
// foundation the CI contrast guard builds on.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { contrastRatio, parseOklch } from "../src/ui/accentContrast.js";

const CSS = readFileSync(resolve(process.cwd(), "src/ui/tokens.css"), "utf8");

// Extract the `--token: oklch(...)` color map from the CSS block that begins at `marker`.
function blockTokens(marker: string): Record<string, string> {
  const start = CSS.indexOf(marker);
  const open = CSS.indexOf("{", start);
  const close = CSS.indexOf("}", open);
  const body = CSS.slice(open, close);
  const map: Record<string, string> = {};
  const re = /--([\w-]+):\s*(oklch\([^;)]*\))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(body)) !== null) map[m[1]] = m[2];
  return map;
}

const THEMES: Array<[string, string]> = [
  ["dark", '[data-identity="bench"],'],
  ["light", '[data-identity="bench"][data-theme="light"],'],
];

// [foreground role, background role, min WCAG ratio]. 4.5 = body text; 3.0 = large/decorative text +
// non-text UI (borders, severity + data marks per WCAG 1.4.11).
const PAIRS: Array<[string, string, number]> = [
  ["text", "bg", 4.5],
  ["text", "surface", 4.5],
  ["text", "surface-2", 4.5],
  ["muted", "bg", 4.5],
  ["muted", "surface", 4.5],
  ["faint", "surface", 3.0],
  ["accent-text", "surface", 4.5],
  ["on-accent", "accent", 4.5],
  ["border-strong", "surface", 3.0],
  ["data-ink", "surface", 3.0],
  ["cost-ok", "surface", 3.0],
  ["cost-warn", "surface", 3.0],
  ["cost-high", "surface", 3.0],
];

describe("role-pair contrast across both themes", () => {
  for (const [theme, marker] of THEMES) {
    const tokens = blockTokens(marker);
    it(`${theme}: every role clears its WCAG bar`, () => {
      for (const [fg, bg, min] of PAIRS) {
        const f = parseOklch(tokens[fg]);
        const b = parseOklch(tokens[bg]);
        expect(f, `${theme} --${fg} present + parseable`).toBeTruthy();
        expect(b, `${theme} --${bg} present + parseable`).toBeTruthy();
        const cr = contrastRatio(f!, b!);
        expect(cr, `${theme}: --${fg} on --${bg} = ${cr.toFixed(2)} (min ${min})`).toBeGreaterThanOrEqual(min);
      }
    });
  }

  it("the brand accent is hue-separated from every severity color (brass never signals warning)", () => {
    // Brand color must never read as warning or default data. Enforce a hue gap
    // between the accent and each severity ramp step in both themes.
    for (const [, marker] of THEMES) {
      const t = blockTokens(marker);
      const accentH = parseOklch(t["accent"])!.H;
      for (const sev of ["cost-ok", "cost-warn", "cost-high"]) {
        const sevH = parseOklch(t[sev])!.H;
        const gap = Math.min(Math.abs(accentH - sevH), 360 - Math.abs(accentH - sevH));
        expect(gap, `accent(${accentH}) vs --${sev}(${sevH}) hue gap`).toBeGreaterThanOrEqual(60);
      }
    }
  });
});
