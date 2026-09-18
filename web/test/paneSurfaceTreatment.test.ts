// Source-check the pane, surface, and chart CSS rules that complement the browser suite:
//   • the canvas is not forced into a fixed document measure;
//   • content stays opaque over the macOS vibrancy material;
//   • flat elevation — the Pulse workspace has no border/shadow wrapper;
//   • warning modules use severity tokens, never the brand accent.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const WEB = process.cwd();
const app = readFileSync(resolve(WEB, "src/ui/app.css"), "utf8");
const desktop = readFileSync(resolve(WEB, "src/ui/desktop.css"), "utf8");
const workspaces = readFileSync(resolve(WEB, "src/ui/workspaces.css"), "utf8");

// Body of the first CSS rule whose selector is exactly `sel` at line start.
function ruleBody(css: string, sel: string): string {
  const esc = sel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return css.match(new RegExp("(?:^|\\n)\\s*" + esc + "\\s*\\{([^}]*)\\}"))?.[1] ?? "";
}

describe("pane, surface, and chart treatment", () => {
  it("the canvas has no fixed document-width cap, so dense desktop fills width", () => {
    const main = ruleBody(app, ".main");
    expect(main, "expected a .main rule").toBeTruthy();
    expect(main).toMatch(/min-width:\s*0/);
    expect(main).not.toMatch(/max-width/);
  });

  it("prose still has a readable per-element measure (line length governed locally, not by a document cap)", () => {
    expect(ruleBody(app, ".caption")).toMatch(/max-width:\s*\d+ch/);
  });

  it("content stays opaque over the macOS vibrancy material", () => {
    // Shell furniture may tint; the content pane must not be translucent over the desktop wallpaper.
    expect(desktop).toMatch(/html\[data-material="vibrancy"\]\s*\.main\s*\{[^}]*background:\s*var\(--bg\)/);
    // Reduce-transparency re-opaques everything (belt-and-suspenders).
    expect(desktop).toMatch(/prefers-reduced-transparency:\s*reduce/);
  });

  it("flat elevation: Pulse is a flowing workspace, not a bordered or elevated wrapper", () => {
    const pulse = ruleBody(workspaces, ".pulse");
    expect(pulse).toMatch(/display:\s*flex/);
    expect(pulse).not.toMatch(/\bborder:|box-shadow:|background:/);
    // Shadow is reserved for genuinely floating surfaces.
    expect(ruleBody(app, ".palette")).toMatch(/box-shadow:/);
  });

  it("warning modules use a severity token, never the brand accent", () => {
    const warning = ruleBody(app, ".compare-warning");
    expect(warning).toMatch(/var\(--cost-warn\)/);
    expect(warning).not.toMatch(/var\(--accent\)/);
  });
});
