// Coarse host-OS class for OS-aware desktop chrome — e.g. the macOS traffic-light
// reserve only applies on macOS. Derived from the userAgent (no Tauri os-plugin dependency); coarse
// but sufficient to gate window-control reserves. A future upgrade could swap in the Tauri os plugin.
/// Classify an OS from a userAgent string. Deterministic; case-insensitive.
export function osClass(ua) {
    const s = ua.toLowerCase();
    if (s.includes("mac os") || s.includes("macintosh"))
        return "macos";
    if (s.includes("windows"))
        return "windows";
    if (s.includes("linux") || s.includes("x11"))
        return "linux";
    return "other";
}
/// Stamp `data-os` on the document root so OS-aware CSS (desktop.css) can gate chrome like the
/// traffic-light reserve. Call once at desktop boot. Defaults to the live `navigator.userAgent`.
///
/// Also stamps `data-material="flat"` as the pre-paint DEFAULT unless it is already
/// set — the native window material (vibrancy on macOS, Mica on Win11) is only knowable in Rust, so
/// the shell injects the real value before the bundle runs; if that injection is absent or fails we
/// degrade to the mandatory opaque flat render. We never stamp `data-de` (not derivable without
/// native code — deferred) or `data-controls` (1:1 with `data-os`, so it would drive nothing).
export function applyOsClass(ua = typeof navigator !== "undefined" ? navigator.userAgent : "", root = document.documentElement) {
    // Prefer an authoritative `data-os` injected by the native pre-boot init script; never overwrite
    // it with UA sniffing. UA is the browser-transport fallback only,
    // when no native shell injected the trait.
    const injected = root.dataset.os;
    const os = injected === "macos" || injected === "windows" || injected === "linux"
        ? injected
        : osClass(ua);
    root.dataset.os = os;
    if (!root.dataset.material)
        root.dataset.material = "flat";
    return os;
}
/// The resolved host OS from the already-stamped `data-os` (authoritative, set by applyOsClass at
/// boot), falling back to a UA sniff. Read this from UI that must gate on the platform — e.g. the
/// background-on-close toggle is Windows/Linux-only (macOS always keeps running).
export function currentOs(root = document.documentElement, ua = typeof navigator !== "undefined" ? navigator.userAgent : "") {
    const injected = root.dataset.os;
    return injected === "macos" || injected === "windows" || injected === "linux"
        ? injected
        : osClass(ua);
}
/// The window material tare should request for this environment. macOS under Tauri
/// gets vibrancy — UNLESS the user asked for Reduce Transparency, where the mandatory flat render is
/// the contrast reference and vibrancy would be a regression. Everything else stays flat: Windows Mica
/// is hardware-gated (not yet), Linux stays flat by design, and the browser has no window material.
/// Rust may later override this with an authoritative value (it alone knows if the effect applied).
/// Vibrancy is ALSO disabled under Increase Contrast: high contrast wants
/// the flat, high-legibility surface, not a translucent one.
export function preferredMaterial(os, isTauri, reduceTransparency, highContrast = false) {
    return os === "macos" && isTauri && !reduceTransparency && !highContrast ? "vibrancy" : "flat";
}
