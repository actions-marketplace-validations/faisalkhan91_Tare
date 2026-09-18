// Persistent navigation uses compact location markers, not selected-state fills. This source-level
// contract covers every navigation treatment so a new blue/neutral pill cannot quietly reappear.

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const WEB = process.cwd();
const app = readFileSync(resolve(WEB, "src/ui/app.css"), "utf8");
const components = readFileSync(resolve(WEB, "src/ui/components.css"), "utf8");
const workspaces = readFileSync(resolve(WEB, "src/ui/workspaces.css"), "utf8");
const tokens = readFileSync(resolve(WEB, "src/ui/tokens.css"), "utf8");

/** Body of the first CSS rule whose selector is exactly `sel` at line start. */
function ruleBody(css: string, sel: string): string {
  const esc = sel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return css.match(new RegExp("(?:^|\\n)\\s*" + esc + "\\s*\\{([^}]*)\\}"))?.[1] ?? "";
}

function forcedColorsBlocks(css: string): string {
  const blocks: string[] = [];
  const header = /@media\s*\(forced-colors:\s*active\)\s*\{/g;
  for (const match of css.matchAll(header)) {
    const start = (match.index ?? 0) + match[0].length;
    let depth = 1;
    let end = start;
    while (end < css.length && depth > 0) {
      if (css[end] === "{") depth += 1;
      else if (css[end] === "}") depth -= 1;
      end += 1;
    }
    blocks.push(css.slice(start, end - 1));
  }
  return blocks.join("\n");
}

function expectNoPersistentFill(body: string): void {
  expect(body).not.toMatch(/--state-selected|--accent-weak|--surface-2/);
  expect(body).not.toMatch(/box-shadow|gradient/);
}

describe("persistent navigation distinguishes location without a selected pill", () => {
  const locationRows = [
    { name: "primary sidebar", css: app, active: ".nav-item.active", baseMarker: ".nav-item::before", marker: ".nav-item.active::before" },
    { name: "run navigator", css: workspaces, active: ".run-profile-run-link.active", baseMarker: ".run-profile-run-link::before", marker: ".run-profile-run-link.active::before" },
  ];

  it.each(locationRows)("uses a short 3×18 brass marker for $name", ({ css, active, baseMarker, marker }) => {
    const selected = ruleBody(css, active);
    const geometry = ruleBody(css, baseMarker);
    const indicator = ruleBody(css, marker);
    expect(selected, `expected ${active}`).toBeTruthy();
    expectNoPersistentFill(selected);
    expect(selected).toMatch(/font-weight:\s*var\(--fw-semibold\)/);
    expect(geometry).toMatch(/width:\s*3px/);
    expect(geometry).toMatch(/height:\s*18px/);
    expect(geometry).toMatch(/border-radius:\s*var\(--radius-pill\)/);
    expect(indicator).toMatch(/background:\s*var\(--brass\)/);
  });

  const lineTabs = [
    { name: "Settings sections", css: components, active: '.settings-nav-button[aria-current="location"]', line: '.settings-nav-button[aria-current="location"]::after', baseLine: ".settings-nav-button::after" },
    { name: "Trust sections", css: components, active: '.trust-nav a[aria-current="page"]', line: '.trust-nav a[aria-current="page"]::after', baseLine: ".trust-nav a::after" },
    { name: "Investigate entities", css: workspaces, active: ".inv-mode.active", line: ".inv-mode.active::after", baseLine: ".inv-mode::after" },
    { name: "Run Profile views", css: workspaces, active: ".run-profile-tab.active", line: ".run-profile-tab.active::after", baseLine: ".run-profile-tab::after" },
    { name: "Optimize lifecycle", css: workspaces, active: ".optimize-view.active", line: ".optimize-view.active::after", baseLine: ".optimize-view::after" },
  ];

  it.each(lineTabs)("uses a compact 2px brass line for $name", ({ css, active, line, baseLine }) => {
    const selected = ruleBody(css, active);
    const geometry = ruleBody(css, baseLine);
    const indicator = ruleBody(css, line);
    expect(selected, `expected ${active}`).toBeTruthy();
    expectNoPersistentFill(selected);
    expect(selected).toMatch(/font-weight:\s*var\(--fw-semibold\)/);
    expect(geometry).toMatch(/height:\s*2px/);
    expect(geometry).toMatch(/border-radius:\s*var\(--radius-pill\)/);
    expect(indicator).toMatch(/background:\s*var\(--brass\)/);
  });

  it("retains every current-location indicator in forced-colors mode", () => {
    const cases = [
      { css: app, selectors: [".nav-item.active::before"] },
      { css: components, selectors: ['.settings-nav-button[aria-current="location"]::after', '.trust-nav a[aria-current="page"]::after'] },
      { css: workspaces, selectors: [".inv-mode.active::after", ".run-profile-run-link.active::before", ".run-profile-tab.active::after", ".optimize-view.active::after"] },
    ];
    for (const { css, selectors } of cases) {
      const forced = forcedColorsBlocks(css);
      expect(forced).toMatch(/background:\s*Highlight/);
      for (const selector of selectors) expect(forced).toContain(selector);
    }
  });

  it("keeps fills for transient data focus and pressed choices", () => {
    const listSelection = ruleBody(app, "[data-nav-id].nav-selected");
    expect(listSelection).toMatch(/background:\s*var\(--state-selected\)/);
    expect(listSelection).toMatch(/box-shadow:\s*inset 2px 0 0 var\(--accent\)/);
    expect(ruleBody(workspaces, ".pulse-now-row.selected")).toMatch(/background:\s*var\(--surface-2\)/);
    expect(ruleBody(workspaces, ".inv-facet-row.active")).toMatch(/background:\s*var\(--surface-2\)/);
    expect(ruleBody(app, ".palette-item.active")).toMatch(/background:\s*var\(--accent-weak\)/);
    expect(ruleBody(app, ".flame-diff-modes button.active")).toMatch(/background:\s*var\(--surface-2\)/);
  });

  it("defines a contrast-checked brass marker in both themes", () => {
    const brassDecls = tokens.match(/--brass:\s*oklch\([^)]*\)/g) ?? [];
    expect(brassDecls.length, "at least a dark + a light --brass").toBeGreaterThanOrEqual(2);
    for (const declaration of brassDecls) expect(declaration).toMatch(/\s78\)/);
  });
});
