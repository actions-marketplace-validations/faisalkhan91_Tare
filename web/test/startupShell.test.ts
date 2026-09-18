// Pre-data startup shell + no-flash boot. Verifies the
// classic pre-paint theme resolver, the static startup shell → mountApp replacement, first-paint
// instrumentation, and that both HTML entrypoints wire them CSP-safely.

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { mountApp } from "../src/main.js";
import { fakeClient } from "./fakeClient.js";
import { setOnboarded } from "../src/ui/prefs.js";

const WEB = process.cwd();

describe("pre-paint theme resolver (prepaint.ts, /)", () => {
  // Execute the ACTUAL shipped script (plain-JS source, no imports) as a classic script would.
  const src = readFileSync(resolve(WEB, "src/prepaint.ts"), "utf8");
  const runPrepaint = (): void => {
    new Function(src)();
  };

  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });
  afterEach(() => vi.unstubAllGlobals());

  it("resolves an explicit light/dark preference straight through", () => {
    localStorage.setItem("tare-theme", "light");
    runPrepaint();
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    localStorage.setItem("tare-theme", "dark");
    runPrepaint();
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("System (or fresh install) follows the OS appearance", () => {
    vi.stubGlobal("matchMedia", (q: string) => ({ matches: q.includes("light") }));
    localStorage.setItem("tare-theme", "system");
    runPrepaint();
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    // Fresh install (no stored value) also follows the OS.
    localStorage.clear();
    vi.stubGlobal("matchMedia", (q: string) => ({ matches: q.includes("dark") ? false : false }));
    runPrepaint();
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
  });

  it("sets a theme even when storage/matchMedia are unavailable (no unset flash state)", () => {
    document.documentElement.removeAttribute("data-theme");
    runPrepaint();
    expect(["light", "dark"]).toContain(document.documentElement.getAttribute("data-theme"));
  });
});

describe("static startup shell + first-paint", () => {
  beforeEach(() => setOnboarded(true));

  it("mountApp clears the static startup shell and replaces it with the real shell", async () => {
    const root = document.createElement("div");
    root.innerHTML =
      '<div class="shell startup-shell" data-startup-shell aria-busy="true"><main class="main"></main></div>';
    document.body.appendChild(root);
    performance.clearMarks?.("tare:shell-ready");
    await mountApp(root, fakeClient());
    // The pre-hydration skeleton is gone; the real shell (with the rail nav) is mounted.
    expect(root.querySelector("[data-startup-shell]")).toBeNull();
    expect(root.querySelector(".shell .sidebar")).toBeTruthy();
    expect(root.querySelector(".statusbar")?.getAttribute("aria-label")).toBe("Workspace status");
    root.remove();
  });

  it("instruments the shell's first paint with a performance mark", async () => {
    performance.clearMarks?.("tare:shell-ready");
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, fakeClient());
    expect(performance.getEntriesByName?.("tare:shell-ready").length ?? 0).toBeGreaterThan(0);
    root.remove();
  });
});

describe("HTML boot wiring (CSP-safe, no-flash)", () => {
  const html = (f: string) => readFileSync(resolve(WEB, f), "utf8");
  for (const entry of ["index.html", "index.tauri.html"]) {
    it(`${entry}: prepaint is a classic render-blocking script before the stylesheets, and a static shell exists`, () => {
      const doc = html(entry);
      // Pre-paint must be a CLASSIC script (no type=module — modules defer past first paint) and must
      // precede the first stylesheet link so data-theme is set before paint.
      const prepaintIdx = doc.indexOf('src="./prepaint.js"');
      const firstCssIdx = doc.indexOf('href="./ui/');
      expect(prepaintIdx, `${entry} loads prepaint.js`).toBeGreaterThan(-1);
      expect(prepaintIdx).toBeLessThan(firstCssIdx);
      expect(/<script[^>]*src="\.\/prepaint\.js"[^>]*>/.exec(doc)?.[0]).not.toMatch(/type=/);
      // Static semantic startup shell inside #app.
      expect(doc).toMatch(/id="app"[\s\S]*data-startup-shell/);
    });
  }
});
