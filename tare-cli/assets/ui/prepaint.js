"use strict";
// Pre-paint theme resolver. Loaded as a CLASSIC render-blocking
// <script src> in <head> — CSP script-src 'self'-safe (not inline), and NOT a module (module scripts
// defer past first paint). It resolves <html data-theme> synchronously from the persisted `tare-theme`
// preference (system|light|dark) + the OS appearance BEFORE the stylesheets paint, so the app never
// flashes the wrong theme. No imports (stays a classic script); written in plain-JS style so it is
// directly executable in tests. Mirrors ui/theme.ts `resolveTheme` (which is unit-tested) — the two
// must agree; keep them in sync.
(function () {
    // Stamp the brand identity FIRST, unconditionally. The entire light palette lives behind
    // `[data-identity="bench"][data-theme="light"]` in tokens.css, so setting data-theme alone left a
    // light-preference user's static startup shell painting the :root DARK tokens (plus
    // `color-scheme: dark`) until main.ts stamped the identity from a deferred module — i.e. every
    // light-mode launch flashed dark, which is exactly what this file exists to prevent. There is only
    // ONE identity (theme.ts `Identity = "bench"`), so this needs no storage
    // read and cannot fail — it stays outside the try/catch below.
    document.documentElement.setAttribute("data-identity", "bench");
    try {
        var saved = localStorage.getItem("tare-theme");
        var pref = saved === "light" || saved === "dark" || saved === "system" ? saved : "system";
        var theme = pref === "light"
            ? "light"
            : pref === "dark"
                ? "dark"
                : typeof matchMedia !== "undefined" &&
                    matchMedia("(prefers-color-scheme: light)").matches
                    ? "light"
                    : "dark";
        document.documentElement.setAttribute("data-theme", theme);
    }
    catch (e) {
        // Storage/matchMedia unavailable (sandbox): fall back to the app's default surface.
        document.documentElement.setAttribute("data-theme", "dark");
    }
})();
