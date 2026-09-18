// A framework-free, byte-stable parallel-coordinates SVG: one vertical axis per
// dimension, one polyline per row. Each axis is normalized independently over the row set, so a
// row's height on each axis is its rank within that dimension. Rows carry an optional tone class
// (cost-ok / cost-warn / cost-high) for the stroke. Deterministic — no clock, no random.
//
// Hover behavior (dim non-hovered lines) is attached to the live DOM here, but the initial markup
// is deterministic so snapshot tests stay stable.
const NS = "http://www.w3.org/2000/svg";
/// Build a parallel-coordinates plot. `axes` are the axis labels (bottom); each row's `values`
/// align to them. Returns an <svg>. A row whose value equals the axis min sits at the bottom.
export function parcoords(axes, rows, opts = {}) {
    const W = opts.width ?? 480;
    const H = opts.height ?? 220;
    const PAD_X = 48;
    const PAD_TOP = 12;
    const PAD_BOT = 44;
    const n = Math.max(1, axes.length);
    const plotH = H - PAD_TOP - PAD_BOT;
    // Per-axis min/max over the rows (independent normalization).
    const mins = axes.map((_, i) => Math.min(...rows.map((r) => r.values[i] ?? 0)));
    const maxs = axes.map((_, i) => Math.max(...rows.map((r) => r.values[i] ?? 0)));
    const axisX = (i) => (n === 1 ? PAD_X : PAD_X + ((W - 2 * PAD_X) * i) / (n - 1));
    const valueY = (i, v) => {
        const span = maxs[i] - mins[i];
        const frac = span <= 0 ? 0.5 : (v - mins[i]) / span; // flat axis → mid-line
        return PAD_TOP + plotH * (1 - frac);
    };
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("class", "parcoords");
    svg.setAttribute("width", String(W));
    svg.setAttribute("height", String(H));
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", opts.ariaLabel ?? "Parallel-coordinates plot");
    // No rows → a centered label rather than a bare axis frame.
    if (rows.length === 0) {
        const t = document.createElementNS(NS, "text");
        t.setAttribute("class", "chart-empty");
        t.setAttribute("x", String(W / 2));
        t.setAttribute("y", String(H / 2));
        t.setAttribute("text-anchor", "middle");
        t.setAttribute("dominant-baseline", "middle");
        t.textContent = "No data";
        svg.appendChild(t);
        return svg;
    }
    // Vertical axes + labels.
    axes.forEach((label, i) => {
        const x = Math.round(axisX(i));
        const line = document.createElementNS(NS, "line");
        line.setAttribute("class", "pc-axis");
        line.setAttribute("x1", String(x));
        line.setAttribute("y1", String(PAD_TOP));
        line.setAttribute("x2", String(x));
        line.setAttribute("y2", String(PAD_TOP + plotH));
        svg.appendChild(line);
        const t = document.createElementNS(NS, "text");
        t.setAttribute("class", "pc-axis-label");
        t.setAttribute("x", String(x));
        t.setAttribute("y", String(H - PAD_BOT + 14));
        // First/last labels anchor inward (start/end) so they don't overflow the viewBox edges
        // (mirrors the tick anchoring).
        t.setAttribute("text-anchor", i === 0 ? "start" : i === axes.length - 1 ? "end" : "middle");
        // Truncate to the per-axis spacing so many/long labels don't overlap their neighbors
        // (~7px per char). First/last get the full column width.
        const spacing = n > 1 ? (W - 2 * PAD_X) / (n - 1) : W - 2 * PAD_X;
        const maxChars = Math.max(4, Math.floor(spacing / 7));
        t.textContent = label.length > maxChars ? `${label.slice(0, maxChars - 1)}…` : label;
        svg.appendChild(t);
        // Per-axis numeric min/max ticks: each axis normalizes independently, so without a
        // stated range a line's height is uninterpretable. The caller formats per unit and returns "" to
        // suppress on binary/rank axes where a numeric min/max is meaningless.
        if (opts.axisFormat) {
            const anchor = i === 0 ? "start" : i === n - 1 ? "end" : "middle";
            const hi = opts.axisFormat(i, maxs[i]);
            const lo = opts.axisFormat(i, mins[i]);
            if (hi) {
                const th = document.createElementNS(NS, "text");
                th.setAttribute("class", "pc-axis-tick");
                th.setAttribute("x", String(x));
                th.setAttribute("y", String(PAD_TOP - 3));
                th.setAttribute("text-anchor", anchor);
                th.textContent = hi;
                svg.appendChild(th);
            }
            if (lo && maxs[i] !== mins[i]) {
                const tl = document.createElementNS(NS, "text");
                tl.setAttribute("class", "pc-axis-tick");
                tl.setAttribute("x", String(x));
                tl.setAttribute("y", String(PAD_TOP + plotH + 10));
                tl.setAttribute("text-anchor", anchor);
                tl.textContent = lo;
                svg.appendChild(tl);
            }
        }
    });
    // One polyline per row.
    for (const r of rows) {
        const pts = axes
            .map((_, i) => `${Math.round(axisX(i))},${Math.round(valueY(i, r.values[i] ?? 0))}`)
            .join(" ");
        const poly = document.createElementNS(NS, "polyline");
        poly.setAttribute("class", `pc-line${r.tone ? ` ${r.tone}` : ""}`);
        poly.setAttribute("points", pts);
        poly.setAttribute("fill", "none");
        const title = document.createElementNS(NS, "title");
        title.textContent = r.label;
        poly.appendChild(title);
        // Hover dims the others — live DOM only (class toggled on the svg), so markup stays stable.
        poly.addEventListener("mouseenter", () => svg.classList.add("pc-hovering"));
        poly.addEventListener("mouseleave", () => svg.classList.remove("pc-hovering"));
        poly.addEventListener("mouseenter", () => poly.classList.add("pc-hot"));
        poly.addEventListener("mouseleave", () => poly.classList.remove("pc-hot"));
        svg.appendChild(poly);
    }
    return svg;
}
