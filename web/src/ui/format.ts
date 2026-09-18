// Display formatters. Two layers, never conflated (see docs/DESIGN_GUIDE.md):
//   - EXACT: `toDollarString` / `fmtSignedDollars` (full micro-USD, byte-identical to the Rust
//     core) — used by the SVG renderers, exports, and `title=` tooltips. Never abbreviated.
//   - DISPLAY: `fmtUsd` / `fmtSignedUsd` — concise on-screen money. Everything else here is
//     presentation-only (tabular figures) and never touches the cost path.

import { toDollarString } from "../money.js";

export { toDollarString };

const MICROS_PER_USD = 1_000_000;
const SUBCENT = 10_000; // micros below $0.01
const COMPACT_AT = 1_000_000 * MICROS_PER_USD; // abbreviate at >= $1,000,000

// Locale-aware, standards-based (ECMA-402). 2dp is the ISO 4217 default for USD; six-decimal
// currency is never shown on screen. Built once; reused.
const usd2 = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });
const usdCompact = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  notation: "compact",
  maximumFractionDigits: 1,
});

/// Concise on-screen USD. 2dp by default ($12.40); a nonzero sub-cent value is trimmed to 2
/// significant figures ($0.0016) rather than six fixed decimals; very large values are compacted
/// ($1.2M). The exact micro-USD value stays available via `toDollarString` (tooltip / exports).
export function fmtUsd(micros: number): string {
  const m = Math.trunc(micros);
  if (!Number.isFinite(m)) return "N/A"; // NaN/±Infinity never render as "$NaN"
  if (m === 0) return "$0.00";
  const abs = Math.abs(m);
  const dollars = abs / MICROS_PER_USD;
  let body: string;
  if (abs < SUBCENT) {
    body = "$" + dollars.toLocaleString("en-US", { maximumSignificantDigits: 2 });
  } else if (abs >= COMPACT_AT) {
    body = usdCompact.format(dollars);
  } else {
    body = usd2.format(dollars);
  }
  return m < 0 ? "-" + body : body;
}

/// Concise signed delta (e.g. +$1.50 / -$0.40), for diffs and burn rate.
export function fmtSignedUsd(micros: number): string {
  const m = Math.trunc(micros);
  return (m < 0 ? "-" : "+") + fmtUsd(Math.abs(m));
}

/// Token count with thousands separators (e.g. 12345 -> "12,345").
export function fmtTokens(n: number): string {
  return Math.trunc(n)
    .toString()
    .replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/// Observed latency, compactly: ms under 1s, else seconds with one decimal (e.g. "850 ms", "2.4 s").
export function fmtDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

/// Exact signed delta dollars (full precision) for data/tooltips and tests.
export function fmtSignedDollars(micros: number): string {
  const s = toDollarString(Math.abs(micros));
  return (micros < 0 ? "-" : "+") + s;
}

export function fmtPct(n: number): string {
  return `${n}%`;
}

/// Humanize a snake_case / kebab-case key for display: separators → spaces, first letter
/// capitalized in sentence case. Only the label changes — the underlying config value/key
/// the user selects stays the raw key. `""` → `""`.
export function humanizeKey(key: string): string {
  const s = key.replace(/[_-]+/g, " ").trim();
  return s.length === 0 ? "" : s.charAt(0).toUpperCase() + s.slice(1);
}
