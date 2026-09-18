// Guard: every CSS custom property referenced WITHOUT a fallback must be defined.
//
// An undefined `var(--x)` is not a loud failure — the declaration becomes invalid at computed-value
// time and the property silently falls back to its initial/inherited value. `--radius-pill` was
// referenced at three sites and defined nowhere, so Optimize's status badges computed
// `border-radius: 0` and shipped as sharp rectangles. Nothing caught it: CSS has no compiler and the
// visual difference is easy to miss.
//
// The rule is deliberately narrow — a reference with NO fallback must resolve:
//   var(--radius-pill)            -> must be defined (broken otherwise)
//   var(--titlebar-height, 44px)  -> fine, the fallback IS the contract
// It does not judge naming or values. Comments are stripped first, so discussing a dead token in a
// comment (as shell.css does) is not a violation. Definitions may come from CSS or from JS
// `setProperty`, since a few tokens are computed at runtime.

import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const SRC = join(import.meta.dirname ?? __dirname, "../src");
const CSS_DIR = join(SRC, "ui");

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) walk(p, out);
    else out.push(p);
  }
  return out;
}

/// Strip /* block */ and // line comments so commentary about a token is never read as a reference.
function stripComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, " ").replace(/(^|[^:])\/\/[^\n]*/g, "$1");
}

describe("CSS custom properties", () => {
  const cssFiles = readdirSync(CSS_DIR).filter((f) => f.endsWith(".css"));
  const tsFiles = walk(SRC).filter((f) => f.endsWith(".ts"));
  const sources: Array<{ label: string; text: string }> = [
    ...cssFiles.map((f) => ({ label: `ui/${f}`, text: stripComments(readFileSync(join(CSS_DIR, f), "utf8")) })),
    ...tsFiles.map((f) => ({ label: f.slice(SRC.length + 1), text: stripComments(readFileSync(f, "utf8")) })),
  ];

  it("has stylesheets to check", () => {
    expect(cssFiles.length).toBeGreaterThan(3);
  });

  it("defines every token referenced without a fallback", () => {
    const defined = new Set<string>();
    for (const { text } of sources) {
      for (const m of text.matchAll(/(--[a-zA-Z0-9-]+)\s*:/g)) defined.add(m[1]);
      for (const m of text.matchAll(/setProperty\(\s*["'`](--[a-zA-Z0-9-]+)["'`]/g)) defined.add(m[1]);
    }

    const missing: string[] = [];
    for (const { label, text } of sources) {
      // Capture the token and whatever immediately follows, to tell `var(--x)` from `var(--x, y)`.
      for (const m of text.matchAll(/var\(\s*(--[a-zA-Z0-9-]+)\s*([,)])/g)) {
        const [, token, next] = m;
        if (next === ",") continue; // has a fallback — intentional, and it still renders
        if (defined.has(token)) continue;
        // A token built by interpolation (`var(--cat-${n})`) is captured up to the `${`; accept it
        // when the family exists, since the exact member is only known at runtime.
        if (token.endsWith("-") && [...defined].some((d) => d.startsWith(token))) continue;
        const entry = `${token} (referenced in ${label})`;
        if (!missing.includes(entry)) missing.push(entry);
      }
    }

    expect(
      missing,
      `These tokens are referenced with no fallback and never defined, so the declaration is ` +
        `invalid and silently renders as the initial value. Define them in ui/tokens.css, add a ` +
        `fallback, or fix the reference:\n  ${missing.join("\n  ")}`
    ).toEqual([]);
  });
});
