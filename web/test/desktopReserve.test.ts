import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// Static CI guard: the desktop title band must reserve the top-left for the macOS
// native traffic-light window controls, or brand content tucks under them. Lints the source CSS.
const css = readFileSync(resolve(process.cwd(), "src/ui/desktop.css"), "utf8");
const tauriConfig = JSON.parse(
  readFileSync(resolve(process.cwd(), "../tare-tauri/tauri.conf.json"), "utf8")
) as {
  app?: {
    security?: {
      capabilities?: Array<{
        identifier?: string;
        windows?: string[];
        permissions?: string[];
      }>;
    };
  };
};

const reservePx = (): number => {
  const m = css.match(/--traffic-light-reserve:\s*(\d+)px/);
  expect(m, "desktop.css must define --traffic-light-reserve").toBeTruthy();
  return Number(m![1]);
};

describe("desktop traffic-light reserve", () => {
  it("defines --traffic-light-reserve at the exact live value, matching the gui.rs fallback", () => {
    // Single source of truth for the seam (tare desktop-testing layer): the desktop.css token, the
    // gui.rs `traffic_light_reserve_px` fallback, and the Playwright boundingBox assertion must all
    // agree on one number. Assert the CSS token equals the Rust fallback so a change to one without the
    // other fails here rather than shipping a silent seam regression.
    const cssReserve = reservePx();
    const gui = readFileSync(resolve(process.cwd(), "../tare-tauri/src/gui.rs"), "utf8");
    const fallback = gui.match(/traffic_light_reserve_px\(false, None\)\.unwrap_or\((\d+)(?:\.\d+)?\)/);
    expect(fallback, "gui.rs must define the macOS reserve fallback").toBeTruthy();
    expect(cssReserve, "desktop.css --traffic-light-reserve must equal the gui.rs fallback").toBe(
      Number(fallback![1])
    );
    expect(cssReserve).toBe(96);
  });

  it("applies the reserve to .brand via the token, gated to macOS, with logical padding", () => {
    // Only [data-os="macos"] reserves the leading space, and it uses the shared token.
    expect(css).toMatch(
      /\[data-os="macos"\]\s*\.brand\s*\{[^}]*padding-inline-start:\s*var\(--traffic-light-reserve\)/
    );
  });

  it("does not reserve lead space on non-macOS (Windows/Linux controls sit right, not left)", () => {
    // The ungated .brand rule must set the lead reserve to 0 — the macOS-gated rule adds it back.
    const plain = css.match(/(?:^|\n)\s*\.brand\s*\{([^}]*)\}/);
    expect(plain, "expected a base .brand rule").toBeTruthy();
    const lit = plain![1].match(/padding-(?:inline-start|left):\s*(\d+)px/);
    if (lit) expect(Number(lit[1]), "non-macOS lead reserve should be 0").toBe(0);
  });

  it("makes the title band draggable on macOS ONLY", () => {
    // macOS: the web IS the title bar (Overlay), so brand+topbar drag the window.
    expect(css).toMatch(/\[data-os="macos"\]\s*\.brand,\s*\[data-os="macos"\]\s*\.topbar\s*\{[^}]*-webkit-app-region:\s*drag/);
    // There must be NO ungated drag rule on the raw .brand/.topbar (that dragged the band on Win/Linux
    // beneath their separate native title bar — chrome decision #1).
    expect(css).not.toMatch(/(?:^|\n)\s*\.brand,\s*\n?\s*\.topbar\s*\{[^}]*-webkit-app-region:\s*drag/);
  });

  it("grants start-dragging only to the runtime-created main window", () => {
    const capability = tauriConfig.app?.security?.capabilities?.find(
      (c) => c.identifier === "main-window"
    );
    expect(capability).toBeTruthy();
    expect(capability?.windows).toEqual(["main"]);
    expect(capability?.permissions).toContain("core:window:allow-start-dragging");
  });

  it("reserves no caption space in the web content on Windows/Linux with native decorations", () => {
    // The Overlay-era Windows trailing reserve + Linux insets are gone: the native title bar is a
    // SEPARATE OS-drawn bar above the webview, so tare's toolbar spans full width.
    expect(css).not.toMatch(/--reserve-trail-win/);
    expect(css).not.toMatch(/\[data-os="windows"\]\s*\.topbar\s*\{[^}]*padding-inline-end/);
    expect(css).not.toMatch(/\[data-os="linux"\]\s*\.topbar\s*\{[^}]*padding-inline-end/);
  });

  it("collapses the traffic-light reserve in macOS fullscreen", () => {
    expect(css).toMatch(
      /\[data-os="macos"\]\[data-fullscreen="true"\]\s*\.brand\s*\{[^}]*padding-inline-start:\s*var\(--s-4\)/
    );
  });

  it("vibrancy is shell-only, gated, and reverts under Reduce Transparency", () => {
    // Gated entirely on the data-material="vibrancy" flag (bootTauri sets it only on eligible macOS).
    expect(css).toMatch(/html\[data-material="vibrancy"\]/);
    // The content pane stays fully opaque — never translucent over the wallpaper.
    expect(css).toMatch(
      /html\[data-material="vibrancy"\]\s*\.main\s*\{[^}]*background:\s*var\(--bg\)/
    );
    // Reduce Transparency forces the desktop layer back to an opaque surface.
    expect(css).toMatch(/@media \(prefers-reduced-transparency: reduce\)/);
  });

  it("marks the Windows/Linux chrome UNVALIDATED pending hardware verification", () => {
    expect(css).toMatch(/UNVALIDATED\[(?:win|linux)\](?:\[(?:win|linux)\])*/);
  });
});

describe("desktop selection and cursor model", () => {
  it("does NOT kill text selection app-wide — content stays copyable", () => {
    // The old `* { user-select: none }` made figures / run ids / labels uncopyable (reads as broken,
    // esp. on Windows/Linux). Selection-none must be scoped to chrome, not the universal selector.
    expect(css).not.toMatch(/(?:^|\n)\s*\*\s*\{[^}]*user-select:\s*none/);
  });

  it("scopes non-selection to app chrome (rail / title band / tape), leaving content selectable", () => {
    expect(css).toMatch(
      /\.brand,\s*\n?\s*\.sidebar,\s*\n?\s*\.topbar,\s*\n?\s*\.statusbar\s*\{[^}]*user-select:\s*none/
    );
  });

  it("defaults the cursor to the arrow on macOS ONLY (HIG), not app-wide", () => {
    // Windows/Linux keep the web-standard pointer on links, which is native there.
    expect(css).toMatch(/\[data-os="macos"\]\s*\*\s*\{[^}]*cursor:\s*default/);
    expect(css).not.toMatch(/(?:^|\n)\s*\*\s*\{[^}]*cursor:\s*default/);
  });

  it("keeps the I-beam on editable fields, winning over the macOS arrow-default on specificity", () => {
    // `[data-os="macos"] *` (0,1,0) outweighs a bare `input` (0,0,1), so the editable rule must carry
    // its own [data-os="macos"]-scoped selectors (0,1,1) to take the I-beam back on macOS.
    expect(css).toMatch(/\[data-os="macos"\]\s*input/);
    expect(css).toMatch(
      /\[data-os="macos"\]\s*input,\s*\n?\s*\[data-os="macos"\]\s*textarea,\s*\n?\s*\[data-os="macos"\]\s*\[contenteditable\]\s*\{[^}]*cursor:\s*text/
    );
  });
});
