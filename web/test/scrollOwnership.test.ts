// Source-check the invariants that keep scrolling intentional: the page never scrolls, the shell is
// a fixed viewport frame, and each scroll owner clamps its track and contains overscroll. Browser
// tests cover the rendered behavior at supported widths.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const WEB = process.cwd();
const app = readFileSync(resolve(WEB, "src/ui/app.css"), "utf8");
const workspaces = readFileSync(resolve(WEB, "src/ui/workspaces.css"), "utf8");
const shellCss = readFileSync(resolve(WEB, "src/ui/shell.css"), "utf8");

/// Body of the first CSS rule whose selector list contains `sel` (anchored at line start). Handles a
/// grouped selector like `html,\nbody { … }`.
function ruleBody(css: string, sel: string): string {
  const esc = sel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return css.match(new RegExp("(?:^|\\n)[^{}]*\\b" + esc + "\\b[^{}]*\\{([^}]*)\\}"))?.[1] ?? "";
}

describe("scroll ownership", () => {
  it("the page itself never scrolls — html/body clip overflow", () => {
    // The shell is the frame; a stray overflow must not scroll the whole document under the chrome.
    expect(ruleBody(app, "body")).toMatch(/overflow:\s*hidden/);
  });

  it("the shell is a fixed full-viewport grid frame", () => {
    const shell = app.match(/(?:^|\n)\.shell\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(shell).toMatch(/height:\s*100vh/);
    expect(shell).toMatch(/display:\s*grid/);
  });

  it("stacked/single shells content-size both chrome rows and clamp the main track", () => {
    const adaptive = shellCss.match(/\.shell\[data-layout="stacked"\],[^{]+\{([^}]*)\}/s)?.[1] ?? "";
    // The toolbar may wrap custom date controls. Its row must grow with that content so it cannot
    // paint over the independently scrolling main track below.
    expect(adaptive).toMatch(/grid-template-rows:\s*auto\s+auto\s+minmax\(0,\s*1fr\)\s+28px/);
  });

  it(".main is the single vertical scroll owner and contains its overscroll", () => {
    const main = app.match(/(?:^|\n)\.main\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(main).toMatch(/overflow-y:\s*auto/);
    expect(main).toMatch(/overscroll-behavior:\s*contain/);
  });

  it("the rail owns its own scroll: clamped track, vertical-only, overscroll contained", () => {
    const rail = app.match(/(?:^|\n)\.sidebar\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(rail).toMatch(/overflow-y:\s*auto/);
    expect(rail).toMatch(/overflow-x:\s*hidden/); // never a horizontal route strip
    expect(rail).toMatch(/min-height:\s*0/); // clamp the 1fr grid track so it scrolls internally
    expect(rail).toMatch(/overscroll-behavior:\s*contain/);
  });

  it("the bounded Run Profile explicitly transfers vertical ownership to its independent panes", () => {
    const boundedMain = app.match(/(?:^|\n)\.main\.main-bounded-workbench\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(boundedMain).toMatch(/overflow:\s*hidden/);
    const body = workspaces.match(/(?:^|\n)\.run-profile-body\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(body).toMatch(/min-height:\s*0/);
    expect(body).toMatch(/overflow:\s*hidden/);
    for (const selector of ["run-profile-run-scroll", "run-profile-canvas", "run-profile-inspector"]) {
      const pane = workspaces.match(new RegExp(`(?:^|\\n)\\.${selector}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
      expect(pane, selector).toMatch(/overflow-y:\s*auto/);
      expect(pane, selector).toMatch(/overscroll-behavior:\s*contain/);
    }
  });
});
