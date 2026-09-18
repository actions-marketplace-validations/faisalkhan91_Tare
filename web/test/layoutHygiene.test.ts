// These source checks preserve content-driven skeleton-card grids without row-coupled dead zones.
// Browser tests cover the rendered multi-width behavior.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const WEB = process.cwd();
const app = readFileSync(resolve(WEB, "src/ui/app.css"), "utf8");

/// Body of the first CSS rule whose selector is exactly `sel` at line start.
function ruleBody(sel: string): string {
  const esc = sel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return app.match(new RegExp("(?:^|\\n)" + esc + "\\s*\\{([^}]*)\\}"))?.[1] ?? "";
}

describe("layout hygiene — no dead-space / row-coupling", () => {
  it(".cards is content-driven (align-items:start), not row-coupled into dead zones", () => {
    const cards = ruleBody(".cards");
    expect(cards, "expected a .cards rule").toBeTruthy();
    expect(cards).toMatch(/align-items:\s*start/); // a taller sibling never stretches its neighbours
    expect(cards).not.toMatch(/align-items:\s*stretch/);
  });

  it(".cards reflows by container width (auto-fit minmax), not a hardcoded viewport breakpoint", () => {
    expect(ruleBody(".cards")).toMatch(/repeat\(\s*auto-fit\s*,\s*minmax\(/);
  });

  it("the only explicit align-items:stretch usages are intentional (Beam track / narrow stacking)", () => {
    // Guard: a new stretch-coupled CARD grid would show up here. The known-good stretches are the Beam
    // lane fill (components.css) and narrow-screen form/control stacking (inside @media) — never .cards.
    const cardsIdx = app.indexOf(".cards {");
    const cardsBlockEnd = app.indexOf("}", cardsIdx);
    expect(app.slice(cardsIdx, cardsBlockEnd)).not.toMatch(/align-items:\s*stretch/);
  });
});
