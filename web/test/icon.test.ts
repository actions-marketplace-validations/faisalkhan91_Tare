// Icon system: the inline SVG primitive that replaces platform-variable
// emoji/glyph controls and the amber brand dot. Asserts the acceptance contract — accessible names
// where needed, forced-color-safe currentColor, consistent sizing, and no emoji controls remain in
// the shell source.

import { describe, it, expect } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { icon, calibrationMark } from "../src/ui/icon.js";

describe("icon primitive", () => {
  it("is decorative by default — aria-hidden, not focusable, no accessible name", () => {
    const svg = icon("close");
    expect(svg.namespaceURI).toBe("http://www.w3.org/2000/svg");
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    expect(svg.getAttribute("focusable")).toBe("false");
    expect(svg.hasAttribute("aria-label")).toBe(false);
    expect(svg.getAttribute("role")).toBeNull();
  });

  it("promotes to a standalone accessible image when given a label (role=img + <title> + aria-label)", () => {
    const svg = icon("warning", { label: "Warning" });
    expect(svg.getAttribute("role")).toBe("img");
    expect(svg.getAttribute("aria-label")).toBe("Warning");
    expect(svg.querySelector("title")?.textContent).toBe("Warning");
    expect(svg.hasAttribute("aria-hidden")).toBe(false); // labelled icons are NOT hidden
  });

  it("draws in currentColor so it inherits text color and is forced-colors-safe", () => {
    const svg = icon("settings");
    expect(svg.getAttribute("stroke")).toBe("currentColor");
    expect(svg.getAttribute("fill")).toBe("none");
    // Solid shapes (dots) opt into currentColor fill rather than a hard-coded hue.
    const dot = icon("more").querySelector("circle");
    expect(dot?.getAttribute("fill")).toBe("currentColor");
  });

  it("renders at a consistent size on a fixed 24-grid (default 16, overridable)", () => {
    const d = icon("dot");
    expect(d.getAttribute("viewBox")).toBe("0 0 24 24");
    expect(d.getAttribute("width")).toBe("16");
    expect(d.getAttribute("height")).toBe("16");
    const small = icon("close", { size: 12 });
    expect(small.getAttribute("width")).toBe("12");
    expect(small.getAttribute("height")).toBe("12");
  });

  it("carries the base `icon` class plus any extra, and throws on an unknown name", () => {
    expect(icon("pin").getAttribute("class")).toBe("icon");
    expect(icon("pin", { class: "brand-mark" }).getAttribute("class")).toBe("icon brand-mark");
    // @ts-expect-error — unknown icon name is a dev-time guard.
    expect(() => icon("nope")).toThrow(/unknown/);
  });

  it("calibrationMark is the Beam-derived gauge glyph, decorative, at wordmark scale", () => {
    const mark = calibrationMark();
    expect(mark.getAttribute("class")).toBe("icon brand-mark");
    expect(mark.getAttribute("aria-hidden")).toBe("true"); // the TARE wordmark is the name
    // Graduated gauge ticks — three vertical lines, not a filled circle (never a traffic-light dot).
    expect(mark.querySelectorAll("line").length).toBe(3);
    expect(mark.querySelector("circle")).toBeNull();
  });
});

describe("no emoji/platform-variable glyph controls remain", () => {
  // These control glyphs—✕ ⋯ ⚙ 📌 • ● ⚠ ▸—must no longer be authored as button/label
  // text anywhere in production TypeScript.
  // Stay within a single string literal (no newline) so a documentation comment mentioning a glyph
  // can't false-positive; still catches a control glyph authored in a `text:` value.
  const BANNED = /text:\s*["'`][^"'`\n]*[✕⋯⚙📌●▪◦⚠▸★☆►▼◄]/u;
  const sourceFiles = (directory: string): string[] =>
    readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
      const path = resolve(directory, entry.name);
      if (entry.isDirectory()) return sourceFiles(path);
      return entry.isFile() && entry.name.endsWith(".ts") ? [path] : [];
    });
  for (const file of sourceFiles(resolve(process.cwd(), "src"))) {
    const label = file.slice(process.cwd().length + 1);
    it(`${label} has no glyph set via a control's text: attribute`, () => {
      const src = readFileSync(file, "utf8");
      expect(src).not.toMatch(BANNED);
    });
  }
});
