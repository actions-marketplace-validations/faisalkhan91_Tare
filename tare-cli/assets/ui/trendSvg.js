// Shared trend (stacked daily bars) SVG renderer. Two entry points share one body:
//   • renderTrendSvg     — export/report-bundle. A byte-for-byte port of tare-core/src/svg.rs
//     `render_trend_svg`; the core snapshot test (trend.svg) and this fn must produce identical
//     output for the same TrendReport. Fixed light palette and constants; do not
//     change its bytes.
// • renderTrendSvgThemed — on-screen only. Same geometry, but colors resolve
//     from the live CSS tokens (transparent bg, var(--muted) axes, accent/cost-ramp series) so
//     the chart lives in the app surface instead of a white slab. Not byte-constrained.
// Integer pixel arithmetic only (all values within JS safe-integer range). Keep the EXPORT path
// in lockstep with svg.rs: same constants, same palette, same string templates.
import { toDollarString } from "./money.js";
import { fmtUsd } from "./ui/format.js";
const WIDTH = 960;
const PAD = 12;
const LEGEND_H = 28;
const TREND_PLOT_H = 240;
const TREND_AXIS_H = 16;
// Must match trend_color in tare-core/src/svg.rs.
const PALETTE = [
    "#4e79a7",
    "#f28e2b",
    "#59a14f",
    "#e15759",
    "#b07aa1",
    "#9c755f",
    "#76b7b2",
    "#edc948",
];
function trendColor(i) {
    return PALETTE[i % PALETTE.length];
}
// On-screen series palette: stable categorical ink, so multi-series trends
// read as distinct data categories — NOT the brand accent and NOT the severity ramp (a series is not a
// "warning"). Cycles the six cat hues, then lighter color-mixes for extra series. Screen-only — never
// used by the byte-golden export path.
const THEMED_PALETTE = [
    "var(--cat-1)",
    "var(--cat-2)",
    "var(--cat-3)",
    "var(--cat-4)",
    "var(--cat-5)",
    "var(--cat-6)",
    "color-mix(in srgb, var(--cat-1) 55%, var(--surface))",
    "color-mix(in srgb, var(--cat-2) 55%, var(--surface))",
];
function themedColor(i) {
    return THEMED_PALETTE[i % THEMED_PALETTE.length];
}
const EXPORT_THEME = {
    bg: "#ffffff",
    axisFill: "#1c1c1c",
    legendFill: "#1c1c1c",
    color: trendColor,
};
const SCREEN_THEME = {
    bg: "transparent",
    axisFill: "var(--muted)",
    legendFill: "var(--text)",
    color: themedColor,
    emptyFill: "var(--muted)",
    showScale: true,
};
function esc(s) {
    return s
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;")
        .replace(/'/g, "&#39;");
}
// Largest-remainder split of `total` pixels across equal-weight columns (matches splitPx).
function splitPx(total, weights) {
    const sum = weights.reduce((a, w) => a + w, 0);
    if (sum === 0 || total <= 0)
        return weights.map(() => 0);
    const base = [];
    const rem = [];
    let used = 0;
    for (let i = 0; i < weights.length; i++) {
        const num = total * weights[i];
        const b = Math.floor(num / sum);
        base.push(b);
        rem.push([num % sum, i]);
        used += b;
    }
    let leftover = total - used;
    rem.sort((a, b) => b[0] - a[0] || a[1] - b[1]);
    for (const [, idx] of rem) {
        if (leftover === 0)
            break;
        base[idx] += 1;
        leftover -= 1;
    }
    return base;
}
// Shared render body. `theme` selects the color scheme; geometry/string templates are identical
// across both entry points (the export path passes EXPORT_THEME to reproduce svg.rs byte-for-byte).
function renderTrend(report, theme) {
    const n = report.days.length;
    const legendY = PAD + TREND_PLOT_H + TREND_AXIS_H + 8;
    const fullW = WIDTH + 2 * PAD;
    // Legend item advance (Unicode scalar count, matching Rust chars().count(), #3).
    const legendItemW = (key) => 16 + Array.from(key).length * 7 + 16;
    // Screen-only: the legend wraps to extra rows when the keys don't fit one line, so
    // the SVG height must grow to fit them. Export (no showScale) stays one row → byte-identical to svg.rs.
    const LEGEND_ROW_STEP = 16;
    let legendRows = 1;
    if (theme.showScale) {
        let simX = PAD;
        for (const s of report.series) {
            const w = legendItemW(s.key);
            if (simX > PAD && simX + w > fullW - PAD) {
                legendRows += 1;
                simX = PAD;
            }
            simX += w;
        }
    }
    const height = legendY + LEGEND_H + (legendRows - 1) * LEGEND_ROW_STEP;
    const out = [];
    out.push(`<svg xmlns="http://www.w3.org/2000/svg" width="${fullW}" height="${height}" viewBox="0 0 ${fullW} ${height}" font-family="monospace">`);
    out.push(`<rect x="0" y="0" width="${fullW}" height="${height}" fill="${theme.bg}"/>`);
    const dayTotal = (i) => 
    // Coalesce non-finite per-day values to 0 so one NaN/Infinity can't poison maxTotal and blank
    // the whole chart. Finite data is unaffected, so export bytes stay identical.
    report.series.reduce((acc, s) => acc + (Number.isFinite(s.per_day[i]) ? s.per_day[i] : 0), 0);
    let maxTotal = 0;
    for (let i = 0; i < n; i++)
        maxTotal = Math.max(maxTotal, dayTotal(i));
    if (n === 0 || maxTotal === 0) {
        // Export omits the fill attribute (byte parity with svg.rs); the screen theme adds a legible one.
        const emptyFill = theme.emptyFill ? ` fill="${theme.emptyFill}"` : "";
        out.push(`<text x="12" y="24" font-size="12"${emptyFill}>No usage</text></svg>`);
        return out.join("");
    }
    const widths = splitPx(WIDTH, new Array(n).fill(1));
    const baseline = PAD + TREND_PLOT_H;
    let x = PAD;
    for (let i = 0; i < n; i++) {
        const colW = widths[i];
        let acc = 0;
        for (let si = 0; si < report.series.length; si++) {
            const s = report.series[si];
            const val = s.per_day[i];
            if (!Number.isFinite(val) || val <= 0)
                continue; // guard non-finite
            let seg = Math.floor((val * TREND_PLOT_H) / maxTotal);
            // Screen-only (theme.showScale): floor a nonzero segment to 1px so a tiny spend beside a huge
            // day doesn't round to 0 and read as zero, then clamp the stack to the plot height so those
            // 1px floors cannot overflow it. The export path remains byte-identical.
            if (theme.showScale) {
                if (seg < 1)
                    seg = 1;
                seg = Math.min(seg, Math.max(0, TREND_PLOT_H - acc));
            }
            if (seg <= 0)
                continue;
            const y = baseline - acc - seg;
            const dollars = toDollarString(val);
            const title = `${report.days[i]} · ${s.key} · ${dollars}`;
            out.push(`<g><rect x="${x}" y="${y}" width="${colW}" height="${seg}" fill="${theme.color(si)}"/><title>${esc(title)}</title></g>`);
            acc += seg;
        }
        x += colW;
    }
    // Screen-only y-scale: a drawn $0 baseline (the zero reference from which bars grow), the peak
    // stacked cost per day in dollars above the plot, and an estimated-versus-actual
    // caption. Skipped for EXPORT_THEME so the exported SVG stays byte-identical to svg.rs.
    if (theme.showScale) {
        out.push(`<line x1="${PAD}" y1="${baseline}" x2="${PAD + WIDTH}" y2="${baseline}" stroke="${theme.axisFill}" stroke-opacity="0.4"/>`);
        out.push(`<text x="${PAD}" y="10" font-family="monospace" font-size="9" fill="${theme.axisFill}">${esc(fmtUsd(maxTotal))} peak/day · $0 baseline</text>`);
        out.push(`<text x="${PAD + WIDTH}" y="10" font-family="monospace" font-size="9" fill="${theme.axisFill}" text-anchor="end">${report.estimated ? `Estimated. Pricing ${esc(report.pricing_version)}` : `Actual. Pricing ${esc(report.pricing_version)}`}</text>`);
    }
    const axisY = baseline + 12;
    out.push(`<text x="${PAD}" y="${axisY}" font-family="monospace" font-size="10" fill="${theme.axisFill}">${esc(report.days[0])}</text>`);
    if (n > 1) {
        out.push(`<text x="${PAD + WIDTH}" y="${axisY}" font-family="monospace" font-size="10" fill="${theme.axisFill}" text-anchor="end">${esc(report.days[n - 1])}</text>`);
    }
    let lx = PAD;
    let ly = legendY;
    for (let si = 0; si < report.series.length; si++) {
        const s = report.series[si];
        const w = legendItemW(s.key);
        // Screen-only: wrap to a new row when the next key would overrun the right edge.
        if (theme.showScale && lx > PAD && lx + w > fullW - PAD) {
            lx = PAD;
            ly += LEGEND_ROW_STEP;
        }
        out.push(`<rect x="${lx}" y="${ly}" width="12" height="12" fill="${theme.color(si)}"/><text x="${lx + 16}" y="${ly + 10}" font-family="monospace" font-size="10" fill="${theme.legendFill}">${esc(s.key)}</text>`);
        lx += w;
    }
    out.push("</svg>");
    return out.join("");
}
// Export/report-bundle renderer — byte-for-byte port of svg.rs `render_trend_svg`. Fixed light
// palette; DO NOT change its output (a golden + the Rust snapshot both pin it).
export function renderTrendSvg(report) {
    return renderTrend(report, EXPORT_THEME);
}
// On-screen renderer — same geometry, colors from live CSS tokens.
export function renderTrendSvgThemed(report) {
    return renderTrend(report, SCREEN_THEME);
}
