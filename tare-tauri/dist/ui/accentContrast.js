// Runtime accent contrast math. The system-accent bridge takes an arbitrary OS accent
// (any hue/lightness) and must still meet tare's AA contrast floor — the guide makes contrast a
// FLOOR, not polish. Python's scripts/verify-contrast.py proves the four FIXED brand accents offline,
// but a live OS accent is only knowable at runtime, so we port the SAME luminance/ratio math here to
// gate it. `accentContrast.test.ts` cross-checks this port against verify-contrast.py's numbers on the
// brand accents, keeping the two implementations cross-verified like the SVG export split.
//
// NOTE: luminance() intentionally reproduces verify-contrast.py exactly — OKLab → LMS³ → linear-sRGB
// → clamp → WCAG weights, WITHOUT an sRGB gamma step. It is the project's canonical (approximate)
// luminance for contrast DECISIONS; do not "correct" it or the cross-check breaks. The clamp is
// self-consistent because it uses this same ratio to decide when a floor is met.
const clamp01 = (x) => Math.max(0, Math.min(1, x));
/// Relative luminance of an OKLCH color — a direct port of verify-contrast.py `lum`.
export function luminance({ L, C, H }) {
    const rad = (H * Math.PI) / 180;
    const a = C * Math.cos(rad);
    const b = C * Math.sin(rad);
    const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
    const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
    const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
    const r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    const g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    const bb = -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s;
    return 0.2126 * clamp01(r) + 0.7152 * clamp01(g) + 0.0722 * clamp01(bb);
}
/// WCAG contrast ratio between two OKLCH colors — a port of verify-contrast.py `cr`.
export function contrastRatio(fg, bg) {
    const a = luminance(fg);
    const b = luminance(bg);
    const hi = Math.max(a, b);
    const lo = Math.min(a, b);
    return (hi + 0.05) / (lo + 0.05);
}
// ---- sRGB hex ↔ OKLCH (proper, gamma-correct — used for the incoming OS accent) ----
const srgbToLinear = (c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
/// Parse "#rrggbb" / "#rgb" (with or without leading #) into an OKLCH color. Returns null on garbage.
export function hexToOklch(hex) {
    let h = hex.trim().replace(/^#/, "");
    if (h.length === 3)
        h = h.split("").map((c) => c + c).join("");
    if (!/^[0-9a-fA-F]{6}$/.test(h))
        return null;
    const r = srgbToLinear(parseInt(h.slice(0, 2), 16) / 255);
    const g = srgbToLinear(parseInt(h.slice(2, 4), 16) / 255);
    const b = srgbToLinear(parseInt(h.slice(4, 6), 16) / 255);
    const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
    const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
    const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
    const L = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
    const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
    const bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
    const C = Math.sqrt(a * a + bb * bb);
    let H = (Math.atan2(bb, a) * 180) / Math.PI;
    if (H < 0)
        H += 360;
    return { L, C, H };
}
/// Parse an "oklch(L C H)" / "oklch(L C H / a)" token (e.g. a computed --surface) into OKLCH.
export function parseOklch(str) {
    const m = str.trim().match(/^oklch\(\s*([\d.]+%?)\s+([\d.]+)\s+([\d.]+)/i);
    if (!m)
        return null;
    const L = m[1].endsWith("%") ? parseFloat(m[1]) / 100 : parseFloat(m[1]);
    return { L, C: parseFloat(m[2]), H: parseFloat(m[3]) };
}
export function oklchToCss({ L, C, H }) {
    return `oklch(${L.toFixed(4)} ${C.toFixed(4)} ${H.toFixed(2)})`;
}
/// Nudge an accent's OKLCH lightness (hue + chroma preserved) until it meets `floor` contrast against
/// `surface`. Moves L away from the surface luminance — the direction that increases contrast. Returns
/// null if the floor is unreachable within [0,1] (caller falls back to the brand accent).
export function clampAccentL(accent, surface, floor) {
    if (contrastRatio(accent, surface) >= floor)
        return accent;
    const up = luminance(accent) >= luminance(surface); // accent is the lighter one → raise L
    const step = 0.005;
    let L = accent.L;
    for (let i = 0; i < 220; i++) {
        L = up ? L + step : L - step;
        if (L <= 0 || L >= 1) {
            L = clamp01(L);
            const edge = { ...accent, L };
            return contrastRatio(edge, surface) >= floor ? edge : null;
        }
        const cand = { ...accent, L };
        if (contrastRatio(cand, surface) >= floor)
            return cand;
    }
    return null;
}
const BLACK = { L: 0, C: 0, H: 0 };
const WHITE = { L: 1, C: 0, H: 0 };
/// The legible ink for text/glyphs sitting ON an accent fill: black or white, whichever contrasts more.
export function pickOnAccent(accent) {
    return contrastRatio(WHITE, accent) >= contrastRatio(BLACK, accent) ? WHITE : BLACK;
}
