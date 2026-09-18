// Build manifest integrity. Parses BOTH HTML entrypoints and asserts every
// local asset URL (CSS/script/font) has a real source the build emits — a linked-but-missing asset
// fails here, and an emitted stylesheet that no entry links (an orphan) fails too. The byte-exact
// embed sync (dist == tare-cli/assets/ui == tare-tauri/dist) is verified separately by
// scripts/check-ui-sync.sh; this guards the HTML → source contract independent of build freshness.

import { describe, it, expect } from "vitest";
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { resolve, posix } from "node:path";

const WEB = process.cwd(); // vitest runs from web/
const ENTRIES = ["index.html", "index.tauri.html"];

/// Local (non-remote, non-inline) href/src URLs referenced by an HTML entrypoint.
function localAssetUrls(html: string): string[] {
  const out: string[] = [];
  const re = /(?:href|src)\s*=\s*"([^"]+)"/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(html)) !== null) {
    const u = m[1];
    if (/^(https?:)?\/\//.test(u) || u.startsWith("data:") || u.startsWith("#")) continue;
    out.push(u);
  }
  return out;
}

/// The source file the build emits for a referenced URL: `./ui/x.css` → `src/ui/x.css`;
/// `./boot.js` → `src/boot.ts`; `./assets/…` → `assets/…` (copied verbatim).
function sourceForUrl(url: string): string {
  const clean = url.replace(/^\.?\//, "");
  if (clean.startsWith("assets/")) return clean;
  const src = `src/${clean}`;
  return src.endsWith(".js") ? src.replace(/\.js$/, ".ts") : src;
}

/// Local url(...) targets in a stylesheet (unquoted or quoted), excluding data:/remote.
function cssUrls(css: string): string[] {
  const out: string[] = [];
  const re = /url\(\s*['"]?([^'")]+)['"]?\s*\)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(css)) !== null) {
    const u = m[1].trim();
    if (!u.startsWith("data:") && !/^(https?:)?\/\//.test(u)) out.push(u);
  }
  return out;
}

describe("build manifest", () => {
  for (const entry of ENTRIES) {
    it(`${entry} references only assets that exist in source`, () => {
      const html = readFileSync(resolve(WEB, entry), "utf8");
      const urls = localAssetUrls(html);
      expect(urls.length, `${entry} references at least the stylesheets + boot script`).toBeGreaterThan(0);
      for (const url of urls) {
        const src = sourceForUrl(url);
        expect(
          existsSync(resolve(WEB, src)),
          `${entry} links ${url} but its source ${src} does not exist`
        ).toBe(true);
      }
    });
  }

  it("every emitted stylesheet is linked by an entrypoint (no orphan CSS ships)", () => {
    const css = readdirSync(resolve(WEB, "src/ui")).filter((f) => f.endsWith(".css"));
    const combined = ENTRIES.map((e) => readFileSync(resolve(WEB, e), "utf8")).join("\n");
    for (const f of css) {
      expect(
        combined.includes(`./ui/${f}`),
        `src/ui/${f} is emitted (glob-copied) but no HTML entrypoint links it`
      ).toBe(true);
    }
  });

  it("links the complete stylesheet set; desktop.css is desktop-only", () => {
    const browser = readFileSync(resolve(WEB, "index.html"), "utf8");
    const desktop = readFileSync(resolve(WEB, "index.tauri.html"), "utf8");
    for (const css of ["tokens", "fonts", "app", "shell", "components", "workspaces"]) {
      expect(browser.includes(`./ui/${css}.css`), `index.html links ${css}.css`).toBe(true);
      expect(desktop.includes(`./ui/${css}.css`), `index.tauri.html links ${css}.css`).toBe(true);
    }
    // Desktop-only chrome is not loaded in the browser build.
    expect(desktop.includes("./ui/desktop.css")).toBe(true);
    expect(browser.includes("./ui/desktop.css")).toBe(false);
  });

  it("the build copies every src/ui/*.css and the web/assets tree (package.json)", () => {
    const pkg = JSON.parse(readFileSync(resolve(WEB, "package.json"), "utf8")) as {
      scripts: { build: string };
    };
    const build = pkg.scripts.build;
    // Glob-copy of stylesheets (not a hardcoded subset), and the assets tree.
    expect(build).toContain("src/ui/*.css dist/ui/");
    expect(build).toContain("assets/. dist/assets/");
  });

  // ---- Fonts ----

  it("every url() in a linked stylesheet resolves to a real source asset", () => {
    // url()s are DIST-relative to the stylesheet (dist/ui/*.css); resolve there, then map to source.
    for (const entry of ENTRIES) {
      const html = readFileSync(resolve(WEB, entry), "utf8");
      const cssRefs = localAssetUrls(html).filter((u) => u.endsWith(".css"));
      for (const cssRef of cssRefs) {
        const cssDist = cssRef.replace(/^\.?\//, ""); // e.g. "ui/fonts.css"
        const css = readFileSync(resolve(WEB, sourceForUrl(cssRef)), "utf8");
        for (const url of cssUrls(css)) {
          const targetDist = posix.normalize(posix.join(posix.dirname(cssDist), url));
          const src = sourceForUrl(targetDist);
          expect(
            existsSync(resolve(WEB, src)),
            `${cssDist} references url(${url}) → ${src} which does not exist`
          ).toBe(true);
        }
      }
    }
  });

  it("fonts.css and the vendored WOFF2 files are in exact correspondence (no orphans, no dangling)", () => {
    const css = readFileSync(resolve(WEB, "src/ui/fonts.css"), "utf8");
    const referenced = new Set(cssUrls(css).map((u) => posix.basename(u)));
    const onDisk = readdirSync(resolve(WEB, "assets/fonts/files")).filter((f) => f.endsWith(".woff2"));
    expect(onDisk.length, "at least one WOFF2 is vendored").toBeGreaterThan(0);
    for (const file of onDisk) expect(referenced.has(file), `${file} is referenced by fonts.css`).toBe(true);
    for (const ref of referenced) expect(onDisk.includes(ref), `fonts.css url ${ref} exists on disk`).toBe(true);
  });

  it("each vendored family ships its OFL license text", () => {
    const dir = resolve(WEB, "assets/fonts/licenses");
    const licenses = readdirSync(dir).filter((f) => f.endsWith("-OFL.txt"));
    expect(licenses.length, "Archivo + Atkinson + IBM Plex Mono licenses").toBeGreaterThanOrEqual(3);
    for (const lic of licenses) {
      expect(readFileSync(resolve(dir, lic), "utf8")).toContain("SIL OPEN FONT LICENSE");
    }
  });
});
