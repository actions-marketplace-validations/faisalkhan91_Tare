// A generic, framework-free, byte-stable SVG scatter.
// Points carry a label + an optional tone class (cost-ok / cost-warn / cost-high) so callers convey
// a third dimension by color, and an optional weight (0..1) for a fourth by dot size. Faint Y
// gridlines with tick values give the plot a scale to read against; dots are stroke-rings with a
// translucent fill so overlapping points read as density. Deterministic: no clock, no random,
// integer-rounded geometry — same input, same SVG. Presentation-only.

export interface ScatterPoint {
  x: number;
  y: number;
  /// Hover title (e.g. "run-a: 1.2M tok, $3.40").
  label: string;
  /// Optional CSS tone class appended to the dot (e.g. "cost-high").
  tone?: string;
  /// Optional magnitude in 0..1 → dot radius (density/size encoding). Absent → default radius.
  weight?: number;
}

export interface ScatterOpts {
  width?: number;
  height?: number;
  xLabel?: string;
  yLabel?: string;
  ariaLabel?: string;
  /// Axis tick-value formatters (default: compact, e.g. 1.2k / 3.4M). Callers that know the unit
  /// (dollars, tokens) can pass their own so the ticks read in that unit.
  formatX?: (n: number) => string;
  formatY?: (n: number) => string;
  /// Draw median crosshairs splitting the plot into four quadrants — turns the scatter
  /// into a quadrant chart (e.g. latency×cost: top-right = slow+expensive, bottom-right = cheap+slow).
  /// Off by default so existing callers stay byte-identical.
  quadrant?: boolean;
  /// On-chart labels for the four quadrant regions — what each corner *means*, in the
  /// caller's terms (only the caller knows the axes). Rendered faint in each corner when `quadrant` is
  /// on. Without them the split is unlabelled and the reader must infer the semantics.
  quadrantLabels?: { tr: string; tl: string; br: string; bl: string };
}

const NS = "http://www.w3.org/2000/svg";

/// Compact, deterministic number label (1234 -> "1.2k", 3_400_000 -> "3.4M").
function compact(n: number): string {
  const a = Math.abs(n);
  if (a >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (a >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (a >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(Math.round(n));
}

function svgText(cls: string, x: number, y: number, text: string, anchor?: string): SVGElement {
  const t = document.createElementNS(NS, "text");
  t.setAttribute("class", cls);
  t.setAttribute("x", String(x));
  t.setAttribute("y", String(y));
  if (anchor) t.setAttribute("text-anchor", anchor);
  t.textContent = text;
  return t;
}

/// Build a scatter over `points`. x/y are normalized to the data max (min pinned at 0 so the
/// origin is meaningful — the cost-per-token frontier is a ray from it). Returns an <svg>.
export function scatter(points: ScatterPoint[], opts: ScatterOpts = {}): SVGElement {
  const W = opts.width ?? 320;
  const H = opts.height ?? 180;
  const PAD = 36; // left margin with room for the Y-axis tick labels (a $ figure) so they don't clip off-canvas
  // >= the max dot radius (6) so a max-Y point's dot isn't clipped above the viewBox.
  const TOP = 8;
  const fmtX = opts.formatX ?? compact;
  const fmtY = opts.formatY ?? compact;
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("class", "scatter");
  svg.setAttribute("width", String(W));
  svg.setAttribute("height", String(H));
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", opts.ariaLabel ?? "Scatter plot");

  // No data → a centered label rather than an empty axis frame, matching the
  // flame/trend renderers' convention.
  if (points.length === 0) {
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

  const maxX = Math.max(1, ...points.map((p) => p.x));
  const maxY = Math.max(1, ...points.map((p) => p.y));
  const plotBottom = H - PAD;

  // Faint Y gridlines at the max and the midpoint, each with its tick value — a scale to read the
  // dots against without the heavy chartjunk of a full grid. Drawn first so dots sit above them.
  for (const frac of [1, 0.5]) {
    const gy = Math.round(plotBottom - (plotBottom - TOP) * frac);
    const line = document.createElementNS(NS, "line");
    line.setAttribute("class", "scatter-grid");
    line.setAttribute("x1", String(PAD));
    line.setAttribute("y1", String(gy));
    line.setAttribute("x2", String(W - 4));
    line.setAttribute("y2", String(gy));
    svg.appendChild(line);
    svg.appendChild(svgText("scatter-tick", PAD - 4, gy + 3, fmtY(maxY * frac), "end"));
  }
  // X max tick value at the bottom-right, under the axis.
  svg.appendChild(svgText("scatter-tick", W - 4, plotBottom + 12, fmtX(maxX), "end"));

  const axis = document.createElementNS(NS, "path");
  axis.setAttribute("class", "scatter-axis");
  axis.setAttribute("d", `M${PAD} ${TOP} V${plotBottom} H${W - 4}`);
  axis.setAttribute("fill", "none");
  svg.appendChild(axis);

  if (opts.xLabel) {
    const t = svgText("scatter-axis-label", Math.round((PAD + W) / 2), plotBottom + 22, opts.xLabel, "middle");
    svg.appendChild(t);
  }
  if (opts.yLabel) {
    // Vertical y-axis title in the left margin, centered on the plot. Anchored at a POSITIVE x
    // (~x=10, inside the viewBox) — the old translate(-6,…) put it at negative user-space x, so the
    // outer <svg> clipped it off the left edge and it never showed.
    const t = document.createElementNS(NS, "text");
    t.setAttribute("class", "scatter-axis-label");
    t.setAttribute("transform", `translate(10, ${Math.round((TOP + plotBottom) / 2)}) rotate(-90)`);
    t.setAttribute("text-anchor", "middle");
    t.textContent = opts.yLabel;
    svg.appendChild(t);
  }

  // Optional median crosshairs → four quadrants. Drawn before the dots so they sit above.
  if (opts.quadrant && points.length >= 2) {
    const median = (vals: number[]): number => {
      const s = [...vals].sort((a, b) => a - b);
      const m = Math.floor(s.length / 2);
      return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
    };
    const mx = PAD + ((W - PAD - 8) * median(points.map((p) => p.x))) / maxX;
    const my = plotBottom - ((plotBottom - TOP) * median(points.map((p) => p.y))) / maxY;
    const cross: ReadonlyArray<readonly [number, number, number, number]> = [
      [mx, TOP, mx, plotBottom], // vertical: median x
      [PAD, my, W - 4, my], // horizontal: median y
    ];
    for (const [x1, y1, x2, y2] of cross) {
      const l = document.createElementNS(NS, "line");
      l.setAttribute("class", "scatter-quadrant");
      l.setAttribute("x1", String(Math.round(x1)));
      l.setAttribute("y1", String(Math.round(y1)));
      l.setAttribute("x2", String(Math.round(x2)));
      l.setAttribute("y2", String(Math.round(y2)));
      svg.appendChild(l);
    }
    // On-chart quadrant-region labels: name each corner in the caller's terms.
    const ql = opts.quadrantLabels;
    if (ql) {
      svg.appendChild(svgText("scatter-quadrant-label", W - 6, TOP + 9, ql.tr, "end"));
      svg.appendChild(svgText("scatter-quadrant-label", PAD + 2, TOP + 9, ql.tl, "start"));
      svg.appendChild(svgText("scatter-quadrant-label", W - 6, plotBottom - 4, ql.br, "end"));
      svg.appendChild(svgText("scatter-quadrant-label", PAD + 2, plotBottom - 4, ql.bl, "start"));
    }
  }

  for (const p of points) {
    const cx = PAD + ((W - PAD - 8) * p.x) / maxX;
    const cy = plotBottom - ((plotBottom - TOP) * p.y) / maxY;
    const dot = document.createElementNS(NS, "circle");
    dot.setAttribute("class", `scatter-dot${p.tone ? ` ${p.tone}` : ""}`);
    dot.setAttribute("cx", String(Math.round(cx)));
    dot.setAttribute("cy", String(Math.round(cy)));
    // Size encoding: weight 0..1 → r 3..6; absent → 4 (the historical default).
    const r = p.weight === undefined ? 4 : 3 + Math.round(Math.max(0, Math.min(1, p.weight)) * 3);
    dot.setAttribute("r", String(r));
    const title = document.createElementNS(NS, "title");
    title.textContent = p.label;
    dot.appendChild(title);
    svg.appendChild(dot);
  }
  return svg;
}
