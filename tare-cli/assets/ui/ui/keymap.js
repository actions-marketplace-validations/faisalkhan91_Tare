// Logical Mod-key token: one place that maps a platform-agnostic chord like "Mod+K"
// to the right PHYSICAL modifier per OS — Command on macOS, Control on Windows/Linux — and formats
// it for display. This kills two long-standing bugs at their source:
//   1. `e.metaKey || e.ctrlKey` fired the ⌘K palette on Ctrl-K on macOS too, hijacking the native
//      readline "delete to end of line" chord. Mod requires the OTHER platform's modifier be absent.
//   2. The "⌘K" glyph was hardcoded in the topbar button / hints — wrong on Windows/Linux, where it
//      should read "Ctrl K".
// Framework-free and jsdom-testable; the OS is derived from the same osClass() the data-os stamp uses.
import { osClass } from "./os.js";
function detectOs() {
    return osClass(typeof navigator !== "undefined" ? navigator.userAgent : "");
}
/// Parse "Mod+Shift+P" → {key:"p", mod:true, shift:true, alt:false}. Segments are "+"-separated;
/// the LAST segment is the key, the rest are modifier names (case-insensitive).
function parseChord(chord) {
    const parts = chord.split("+").map((p) => p.trim()).filter(Boolean);
    const key = (parts.pop() ?? "").toLowerCase();
    const names = new Set(parts.map((p) => p.toLowerCase()));
    return { key, mod: names.has("mod"), shift: names.has("shift"), alt: names.has("alt") };
}
/// Does a keydown satisfy a logical chord like "Mod+K"? `Mod` resolves to the Command key on macOS
/// (with Control required UP) and the Control key everywhere else (with Command required UP), so
/// Ctrl-K on macOS — a native chord — no longer triggers a Mod+K binding. When no `Mod` is requested,
/// neither platform modifier may be held.
export function matchShortcut(e, chord, os = detectOs()) {
    const want = parseChord(chord);
    if (e.key.toLowerCase() !== want.key)
        return false;
    if (e.shiftKey !== want.shift)
        return false;
    if (e.altKey !== want.alt)
        return false;
    if (want.mod) {
        return os === "macos" ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
    }
    return !e.metaKey && !e.ctrlKey;
}
/// Render a logical chord for display. macOS uses tight glyphs ("⌘K", "⇧⌘P"); other OSes use spaced
/// words ("Ctrl K", "Ctrl Shift P"). Mirrors what each platform's own menus show.
export function formatShortcut(chord, os = detectOs()) {
    const mac = os === "macos";
    const want = parseChord(chord);
    const mods = [];
    // Canonical macOS glyph order is ⌃⌥⇧⌘; we only use ⇧/⌥/⌘ here, ⌘ last so "⇧⌘P" reads right.
    if (want.alt)
        mods.push(mac ? "⌥" : "Alt");
    if (want.shift)
        mods.push(mac ? "⇧" : "Shift");
    if (want.mod)
        mods.push(mac ? "⌘" : "Ctrl");
    const tokens = [...mods, want.key.toUpperCase()];
    return tokens.join(mac ? "" : " ");
}
