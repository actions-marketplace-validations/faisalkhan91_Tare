// Theme: a CSS custom-property switch on <html data-theme>. The stored
// PREFERENCE is a tri-state (system|light|dark), distinct from the resolved RENDER theme (light|dark).
// "system" follows the OS live; an explicit light/dark override wins and persists. All guards tolerate
// jsdom (no matchMedia / localStorage in some environments).
const KEY = "tare-theme";
function safeStorage() {
    try {
        return typeof localStorage !== "undefined" ? localStorage : null;
    }
    catch {
        return null; // some sandboxes throw on access
    }
}
function prefersLight() {
    try {
        return typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: light)").matches;
    }
    catch {
        return false;
    }
}
/// The stored theme preference. Storage migration is implicit: a MISSING (fresh install) or
/// unrecognized value resolves to "system"; existing "light"/"dark" remain explicit overrides;
/// choosing System persists the literal "system" (never a storage deletion).
export function themePreference() {
    const saved = safeStorage()?.getItem(KEY);
    if (saved === "light" || saved === "dark" || saved === "system")
        return saved;
    return "system";
}
/// Resolve a preference to the render theme. "system" → the current OS appearance.
export function resolveTheme(pref = themePreference()) {
    if (pref === "light" || pref === "dark")
        return pref;
    return prefersLight() ? "light" : "dark";
}
/// The theme to apply at boot: resolve the stored preference (fresh install → OS).
export function initialTheme() {
    return resolveTheme();
}
export function applyTheme(theme, doc = document) {
    doc.documentElement.setAttribute("data-theme", theme);
}
/// Persist a preference and return the resolved render theme to apply. Choosing System stores
/// "system" so the app keeps tracking the OS rather than freezing today's resolved value.
export function setThemePreference(pref) {
    try {
        safeStorage()?.setItem(KEY, pref);
    }
    catch {
        /* ignore */
    }
    return resolveTheme(pref);
}
/// React to OS light/dark changes at runtime — but ONLY while the preference is "system"; an explicit
/// light/dark choice stays authoritative: honor system appearance, but let an explicit
/// choice win). Returns an unsubscribe; a no-op where matchMedia is absent (jsdom/sandboxes).
export function watchOsTheme(onChange) {
    try {
        if (typeof matchMedia === "undefined")
            return () => { };
        const mq = matchMedia("(prefers-color-scheme: light)");
        const handler = (e) => {
            if (themePreference() !== "system")
                return; // explicit override wins over the OS signal
            onChange(e.matches ? "light" : "dark");
        };
        mq.addEventListener?.("change", handler);
        return () => mq.removeEventListener?.("change", handler);
    }
    catch {
        return () => { };
    }
}
const IDENTITY_KEY = "tare-identity";
export function initialIdentity() {
    // Migrate any legacy stored identity, including `brass`, to `bench`. There is one
    // committed identity; the stored value is only cleaned up so a future read never sees stale data.
    try {
        const s = safeStorage();
        const stored = s?.getItem(IDENTITY_KEY);
        if (stored && stored !== "bench")
            s?.setItem(IDENTITY_KEY, "bench");
    }
    catch {
        /* ignore */
    }
    return "bench";
}
export function applyIdentity(id, doc = document) {
    doc.documentElement.setAttribute("data-identity", id);
}
