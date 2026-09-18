// Flamegraph SVG renderer. Two entry points share one geometry body:
//   • renderSvg       — export/report-bundle. A byte-for-byte port of tare-core/src/svg.rs; the core
//     snapshot test and this fn must produce identical output for the same FlamegraphModel. Fixed
//     light palette + constants — do NOT change its bytes (invariant, mirrors trendSvg.ts). ALWAYS
// token-weighted, so its geometry is frozen.
// • renderSvgThemed — on-screen only. Same geometry body, but colors resolve from
// live CSS tokens, and frame WIDTH follows the caller-chosen weight: "cost"
//     (node micros, the honest "where the money went" default) or "tokens" (raw token share).
//     Cache-read and output tokens price differently, so token width ≠ cost — the on-screen default
//     is Cost. NOT byte-constrained.
// Integer pixel arithmetic only. Keep the EXPORT path in lockstep with svg.rs.

import { toDollarString } from "./money.js";

const WIDTH = 960;
const ROW_H = 30;
const PAD = 12;
const LEGEND_H = 28;

export interface FlamegraphNode {
  name: string;
  tokens: number;
  micros: number;
  cache_class?: string;
  children: FlamegraphNode[];
}

export interface FlamegraphModel {
  run_id: string;
  pricing_version: string;
  effective_date: string;
  root: FlamegraphNode;
}

/// Which node quantity drives on-screen frame WIDTH. "cost" weights by estimated
/// micro-USD (the honest "where money went" view); "tokens" weights by raw token share. The export
/// renderer is always token-weighted and byte-frozen; this only affects `renderSvgThemed`.
export type FlameWeight = "cost" | "tokens";

/// The weight quantity for a node under a given mode. Kept tiny so the shared render body and the
/// on-screen cull/sort transforms all agree on what "width" means.
export function flameWeightOf(node: FlamegraphNode, weight: FlameWeight): number {
  return weight === "cost" ? node.micros : node.tokens;
}

// Must match CacheClass::color in tare-core/src/model.rs (the EXPORT palette — byte-locked).
const CLASS_COLOR: Record<string, string> = {
  fresh: "#4e79a7",
  cache_write_5m: "#f28e2b",
  cache_write_1h: "#e15759",
  cache_read: "#59a14f",
  output: "#b07aa1",
  reasoning: "#9c755f",
};

// On-screen cache-class palette: stable categorical ink, using the same
// per-cache-class hue mapping as the composition ribbon (fresh = cat-1, cache-read = cat-2,
// cache-write = cat-3, output = cat-4, reasoning = cat-5) so a category reads the same colour on the
// flamegraph, the ribbon, and everywhere else. Never the brand accent, never the severity ramp.
// Screen-only; never used by the byte-golden export path.
const THEMED_CLASS_COLOR: Record<string, string> = {
  fresh: "var(--cat-1)",
  cache_write_5m: "var(--cat-3)",
  cache_write_1h: "color-mix(in oklch, var(--cat-3), black 15%)",
  cache_read: "var(--cat-2)",
  output: "var(--cat-4)",
  reasoning: "var(--cat-5)",
};

const LEGEND_ORDER = ["fresh", "cache_write_5m", "cache_write_1h", "cache_read", "output", "reasoning"];

// Color scheme for one render. The export path pins the fixed light theme for golden/Rust byte parity;
// the on-screen path uses live CSS tokens. `emptyFill` is optional so the export "No usage" text stays
// byte-identical (no fill attribute) while the screen one is legible.
interface FlameTheme {
  bg: string;
  stroke: string;
  labelFill: string;
  legendFill: string;
  classColor: Record<string, string>;
  defaultFill: string;
  emptyFill?: string;
  /// Screen-only: stamp a pre-order `data-frame` index on each frame so the inspector
  /// can map a hovered rect back to its node. Off for the export path (keeps the bytes golden).
  interactive?: boolean;
}

const EXPORT_THEME: FlameTheme = {
  bg: "#ffffff",
  stroke: "#ffffff",
  labelFill: "#1c1c1c",
  legendFill: "#1c1c1c",
  classColor: CLASS_COLOR,
  defaultFill: "#d9dce3",
};

const SCREEN_THEME: FlameTheme = {
  bg: "transparent",
  stroke: "var(--surface)",
  labelFill: "var(--text)",
  legendFill: "var(--text)",
  classColor: THEMED_CLASS_COLOR,
  defaultFill: "var(--surface-2)",
  emptyFill: "var(--muted)",
  interactive: true,
};

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function depthOf(node: FlamegraphNode): number {
  let max = 0;
  for (const c of node.children) {
    const d = depthOf(c);
    if (d > max) max = d;
  }
  return 1 + max;
}

// Largest-remainder split of `total` pixels across child token weights.
function splitPx(total: number, weights: number[]): number[] {
  const sum = weights.reduce((a, w) => a + w, 0);
  if (sum === 0 || total <= 0) return weights.map(() => 0);
  const base: number[] = [];
  const rem: Array<[number, number]> = [];
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
    if (leftover === 0) break;
    base[idx] += 1;
    leftover -= 1;
  }
  return base;
}

function fillFor(node: FlamegraphNode, theme: FlameTheme): string {
  if (node.cache_class) return theme.classColor[node.cache_class] ?? theme.defaultFill;
  return theme.defaultFill;
}

function renderNode(
  node: FlamegraphNode,
  x: number,
  w: number,
  depth: number,
  out: string[],
  theme: FlameTheme,
  counter: { n: number },
  weight: FlameWeight
): void {
  const idx = counter.n++; // pre-order index, matches flameFrames() ordering
  const y = PAD + depth * ROW_H;
  const h = ROW_H - 2;
  const dollars = toDollarString(node.micros);
  const title = `${node.name} · ${node.tokens} tok · ${dollars}`;
  // Screen-only data-frame index for the inspector; omitted on the export path (byte-golden).
  const frameAttr = theme.interactive ? ` data-frame="${idx}"` : "";
  out.push(
    `<g><rect x="${x}" y="${y}" width="${w}" height="${h}" fill="${fillFor(node, theme)}" stroke="${theme.stroke}" stroke-width="1"${frameAttr}/><title>${esc(title)}</title>`
  );
  const maxChars = Math.floor(Math.max(0, w - 8) / 7);
  if (maxChars >= 3) {
    const chars = Array.from(node.name);
    let label = node.name;
    if (chars.length > maxChars) {
      label = chars.slice(0, Math.max(0, maxChars - 1)).join("") + "…";
    }
    out.push(
      `<text x="${x + 4}" y="${y + 14}" font-family="monospace" font-size="11" fill="${theme.labelFill}">${esc(label)}</text>`
    );
  }
  out.push("</g>");

  if (node.children.length > 0) {
    const weights = node.children.map((c) => flameWeightOf(c, weight));
    const widths = splitPx(w, weights);
    let cx = x;
    for (let i = 0; i < node.children.length; i++) {
      renderNode(node.children[i], cx, widths[i], depth + 1, out, theme, counter, weight);
      cx += widths[i];
    }
  }
}

function renderLegend(y: number, out: string[], theme: FlameTheme): void {
  let x = PAD;
  for (const c of LEGEND_ORDER) {
    out.push(
      `<rect x="${x}" y="${y}" width="12" height="12" fill="${theme.classColor[c]}"/><text x="${x + 16}" y="${y + 10}" font-family="monospace" font-size="10" fill="${theme.legendFill}">${esc(c)}</text>`
    );
    // Unicode scalar count (matching Rust `chars().count()`), not UTF-16 code units.
    x += 16 + Array.from(c).length * 7 + 16;
  }
}

// Shared render body. `theme` selects the color scheme; `weight` selects the width quantity.
// Geometry/string templates are identical across both entry points (the export path passes
// EXPORT_THEME + "tokens" to reproduce svg.rs byte-for-byte). Overall SVG width/height depend only on
// tree depth, so the outer dimensions are the same in either weight mode.
function renderFlame(model: FlamegraphModel, theme: FlameTheme, weight: FlameWeight): string {
  const depth = depthOf(model.root);
  const legendY = PAD + depth * ROW_H + 8;
  const height = legendY + LEGEND_H;
  const fullW = WIDTH + 2 * PAD;
  const out: string[] = [];
  out.push(
    `<svg xmlns="http://www.w3.org/2000/svg" width="${fullW}" height="${height}" viewBox="0 0 ${fullW} ${height}" font-family="monospace">`
  );
  out.push(`<rect x="0" y="0" width="${fullW}" height="${height}" fill="${theme.bg}"/>`);
  if (flameWeightOf(model.root, weight) === 0) {
    const emptyFill = theme.emptyFill ? ` fill="${theme.emptyFill}"` : "";
    // Cost mode over a run that has tokens but no priced cost = unpriced usage. Never fabricate a
    // dollar width; say so and point at Tokens. Export/token mode keeps "No usage".
    const msg =
      weight === "cost" && model.root.tokens > 0
        ? "No priced cost — usage is unpriced (switch to Tokens)"
        : "No usage";
    out.push(`<text x="12" y="24" font-size="12"${emptyFill}>${esc(msg)}</text></svg>`);
    return out.join("");
  }
  renderNode(model.root, PAD, WIDTH, 0, out, theme, { n: 0 }, weight);
  renderLegend(legendY, out, theme);
  out.push("</svg>");
  return out.join("");
}

/// Export/report-bundle renderer — byte-for-byte port of svg.rs. Fixed light palette; ALWAYS
/// token-weighted; DO NOT change its output (a core snapshot pins it).
export function renderSvg(model: FlamegraphModel): string {
  return renderFlame(model, EXPORT_THEME, "tokens");
}

/// On-screen renderer — same geometry body, colors from live CSS tokens. Frame width
/// follows `weight`; the honest default is Cost (micros), since token width ≠ cost.
export function renderSvgThemed(model: FlamegraphModel, weight: FlameWeight = "cost"): string {
  return renderFlame(model, SCREEN_THEME, weight);
}
