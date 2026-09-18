// System-accent bridge applier. Takes the OS accent (an sRGB hex the Rust shell reads
// from NSColor.controlAccentColor / Windows UISettings / the XDG portal and emits) and drives tare's
// affordance tokens with it — focus ring, selection, primary fill, active-nav, engaged chips — but
// only when eligible and only after clamping it to tare's AA contrast floor against the live surface.
//
// Design decisions it enforces (from grilling):
//   • #7 delivery: off by default (systemAccentEligible → the opt-in pref); the committed Calibrated
//     Bench accent is the brand, so the OS accent only tints affordances when explicitly enabled.
//   • #8 reach: sets --accent-system / --on-accent-system / --focus-ring, which the CSS repoints the
//     affordances to; data/chart ramps keep reading --accent, so they stay brand-stable.
//   • accessibility: the OS accent is contrast-clamped; if it cannot be made legible we fall back
//     to the bench accent by simply not overriding. forced-colors/prefers-contrast still win via tokens.css.
//
// Inline custom props on <html> beat the per-identity stylesheet tokens without !important. Absent the
// Rust event (browser build), nothing calls this and the UI stays on the bench accent.
import { hexToOklch, parseOklch, oklchToCss, clampAccentL, pickOnAccent } from "./accentContrast.js";
// Off by default; the OS accent only tints affordances when the user explicitly enables it. The
// contrast clamp remains mandatory for that case.
const OS_ACCENT_KEY = "tare-os-accent";
const RING_FLOOR = 3.0; // rings / large affordances (WCAG non-text)
// Remember the last OS accent so a dark/light switch (which changes --surface) can re-clamp it.
let lastHex = null;
/// The bridge drives affordances only when the user has opted in (off by default — the bench accent is the brand).
export function systemAccentEligible() {
    try {
        return typeof localStorage !== "undefined" && localStorage.getItem(OS_ACCENT_KEY) === "on";
    }
    catch {
        return false;
    }
}
function readSurface(root) {
    try {
        // Live computed value in the browser (the surface comes from the stylesheet); fall back to an
        // inline value (jsdom doesn't resolve custom props from stylesheets, and callers may inject one).
        const v = getComputedStyle(root).getPropertyValue("--surface") || root.style.getPropertyValue("--surface");
        return v ? parseOklch(v) : null;
    }
    catch {
        return null;
    }
}
function clearBridge(root) {
    root.style.removeProperty("--accent-system");
    root.style.removeProperty("--on-accent-system");
    root.style.removeProperty("--focus-ring");
    delete root.dataset.systemAccent;
}
/// Apply an OS accent (sRGB hex) to the affordance tokens, clamped vs the live surface. No-op + clears
/// any prior bridge when the accent is null, the identity is explicit, or the accent can't be made
/// legible (→ bench-accent fallback). Returns true iff the bridge is now active. `root` is injectable for tests.
export function applySystemAccent(hex, root = document.documentElement) {
    lastHex = hex;
    if (!hex || !systemAccentEligible()) {
        clearBridge(root);
        return false;
    }
    const accent = hexToOklch(hex);
    const surface = readSurface(root);
    if (!accent || !surface) {
        clearBridge(root);
        return false;
    }
    const clamped = clampAccentL(accent, surface, RING_FLOOR);
    if (!clamped) {
        clearBridge(root); // unclampable against this surface → keep the brand accent
        return false;
    }
    root.style.setProperty("--accent-system", oklchToCss(clamped));
    root.style.setProperty("--on-accent-system", oklchToCss(pickOnAccent(clamped)));
    root.style.setProperty("--focus-ring", oklchToCss(clamped));
    root.dataset.systemAccent = "on";
    return true;
}
/// Re-clamp the last OS accent against the current surface — call after a theme (dark/light) switch or
/// an identity change. Cheap no-op when no OS accent has arrived.
export function refreshSystemAccent(root = document.documentElement) {
    applySystemAccent(lastHex, root);
}
/// Best-effort wiring to the Rust shell's `system-accent` event (payload: a hex string or {hex}). The
/// Rust emit side is hardware-gated per OS and lands later; on the browser transport this is a no-op.
export function wireSystemAccent(listen) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const l = listen ?? globalThis.__TAURI__?.event?.listen;
    if (typeof l !== "function")
        return;
    l("system-accent", (e) => {
        const p = e.payload;
        const hex = typeof p === "string"
            ? p
            : p && typeof p === "object" && typeof p.hex === "string"
                ? p.hex
                : null;
        applySystemAccent(hex);
    });
}
