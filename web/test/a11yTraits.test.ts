// Native accessibility-trait bridge and per-OS scrollbar scoping. The WebView honors OS
// accessibility media queries; these helpers reflect them onto <html>, quiet inactive chrome, and
// scope the macOS overlay scrollbar. Actual OS settings remain part of native release testing.

import { describe, it, expect, afterEach, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { preferredMaterial } from "../src/ui/os.js";
import { applyA11yTraits, wireWindowActive } from "../src/bootTauri.js";

const WEB = process.cwd();

afterEach(() => vi.unstubAllGlobals());

describe("material respects Reduce Transparency and Increase Contrast", () => {
  it("vibrancy only on macOS+Tauri with neither reduce-transparency nor high-contrast", () => {
    expect(preferredMaterial("macos", true, false, false)).toBe("vibrancy");
    expect(preferredMaterial("macos", true, true, false)).toBe("flat"); // reduce transparency
    expect(preferredMaterial("macos", true, false, true)).toBe("flat"); // high contrast
    expect(preferredMaterial("windows", true, false, false)).toBe("flat");
    expect(preferredMaterial("macos", false, false, false)).toBe("flat"); // browser
  });
});

describe("applyA11yTraits reflects OS traits onto <html>", () => {
  it("sets data-high-contrast under prefers-contrast/forced-colors, clears it otherwise", () => {
    const root = document.createElement("html");
    vi.stubGlobal("matchMedia", (q: string) => ({ matches: q.includes("contrast") }));
    const t = applyA11yTraits(root);
    expect(t.highContrast).toBe(true);
    expect(root.dataset.highContrast).toBe("true");

    vi.stubGlobal("matchMedia", (q: string) => ({ matches: q.includes("reduced-transparency") }));
    const t2 = applyA11yTraits(root);
    expect(t2.reduceTransparency).toBe(true);
    expect(t2.highContrast).toBe(false);
    expect(root.dataset.highContrast).toBeUndefined();
  });
});

describe("wireWindowActive toggles data-inactive on window focus changes", () => {
  it("marks inactive on blur and clears on focus", () => {
    const root = document.createElement("html");
    let cb: ((e: { payload: boolean }) => void) | undefined;
    wireWindowActive(() => ({ onFocusChanged: (fn) => (cb = fn) }), root);
    expect(root.dataset.inactive).toBeUndefined(); // active on load
    cb!({ payload: false });
    expect(root.dataset.inactive).toBe("true");
    cb!({ payload: true });
    expect(root.dataset.inactive).toBeUndefined();
  });
  it("is a no-op when no native window is available (browser transport)", () => {
    const root = document.createElement("html");
    wireWindowActive(() => undefined, root);
    expect(root.dataset.inactive).toBeUndefined();
  });
});

describe("CSS scopes chrome to platform and accessibility traits", () => {
  const desktop = readFileSync(resolve(WEB, "src/ui/desktop.css"), "utf8");
  const app = readFileSync(resolve(WEB, "src/ui/app.css"), "utf8");
  it("the overlay scrollbar is scoped to macOS (Windows/Linux keep native)", () => {
    expect(desktop).toMatch(/\[data-os="macos"\]\s*::-webkit-scrollbar\b/);
    expect(desktop).toMatch(/\[data-os="macos"\]\s*\{[^}]*scrollbar-width/);
    // No unscoped global scrollbar override remains.
    expect(desktop).not.toMatch(/(?:^|\n)::-webkit-scrollbar\b/);
  });
  it("quiets the chrome when the window is inactive", () => {
    expect(desktop).toMatch(/\[data-inactive\]/);
  });
  it("respects high contrast (shared, browser + desktop)", () => {
    expect(app).toMatch(/@media\s*\(prefers-contrast: more\)/);
    expect(app).toMatch(/\[data-high-contrast\]/);
  });
});
