import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// Static CI guard: micro-labels must not stack all-caps + 11px + wide tracking +
// de-emphasis (four readability reducers). The `.label` rule reads sentence case at 12px; hierarchy
// comes from weight/color, not caps. Lints the source CSS.
const css = readFileSync(resolve(process.cwd(), "src/ui/app.css"), "utf8");

// The shared `.label` rule block.
function labelRule(): string {
  const m = css.match(/\.label\s*\{([^}]*)\}/);
  expect(m, "expected a .label rule in app.css").toBeTruthy();
  return m![1];
}

describe("label typography", () => {
  it("does not force all-caps on .label (sentence case aids readability)", () => {
    expect(labelRule()).not.toMatch(/text-transform:\s*uppercase/);
  });

  it("sizes .label at 12px (--fs-sm), above the 11px micro floor", () => {
    expect(labelRule()).toMatch(/font-size:\s*var\(--fs-sm\)/);
  });

  it("keeps weight for hierarchy and drops the wide caps tracking", () => {
    const rule = labelRule();
    expect(rule).toMatch(/font-weight:\s*var\(--fw-semibold\)/); // hierarchy via weight, not caps
    // Tracking is near-zero for sentence case (the 0.05em was for all-caps).
    const ls = rule.match(/letter-spacing:\s*([\d.]+)em/);
    if (ls) expect(Number(ls[1])).toBeLessThanOrEqual(0.02);
  });
});
