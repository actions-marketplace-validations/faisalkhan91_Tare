// Pulse workspace. Answers "is capture healthy, what changed, and what deserves attention?" The
// hierarchy starts with one scoped spend answer, its period delta, the observed/baseline/forecast
// line, and the compact trust strip. Attention, controllable drivers, the Now feed, and the Pulse
// Beam extend that view without changing its scope contract.
//
// Scope discipline: every figure derives from ONE canonical period load
// (`burnrate` for spend + forecast, `coverage` for trust) so they share scope and there is exactly one
// spend answer — never today-vs-period or per-card duplicates. All dollars are ESTIMATED (labelled);
// the forecast band is projected pace, never a fabricated certainty. Framework-free + jsdom-testable.

import { el, rawSvg } from "../ui/el.js";
import { icon } from "../ui/icon.js";
import { errorNode } from "../ui/errorNode.js";
import { fmtUsd } from "../ui/format.js";
import { parseHash, routePath } from "../ui/store.js";
import { serializeAnalysisHash } from "../analysis/store.js";
import { cohortHash } from "../analysis/serialize.js";
import { investigationFromState } from "../analysis/investigation.js";
import { renderNowFeed } from "./pulseNow.js";
import { tareBeam, type BeamModel } from "../ui/tareBeam.js";
import type { Route } from "../ui/store.js";
import type { WorkspaceContext } from "../shell/workbench.js";
import type { CohortDimension, CohortFilter, CohortSpec } from "../analysis/types.js";
import type {
  TareClient,
  BurnRate,
  BurnRateRange,
  Coverage,
  Anomaly,
  PeriodBudget,
  FailureWasteReport,
  LoopWasteReport,
  SavingsLedger,
  EstimateConfidence,
} from "../client.js";

/// Human label for the budget period the figures are scoped to.
function periodLabel(period: string): string {
  return period === "day"
    ? "today"
    : period === "week"
      ? "this week"
      : period === "month"
        ? "this month"
        : period === "year"
          ? "this year"
          : `this ${period}`;
}

const PULSE_RANGES: ReadonlyArray<{ value: BurnRateRange; label: string; accessible: string }> = [
  { value: "day", label: "Day", accessible: "Today" },
  { value: "week", label: "Week", accessible: "Week to date" },
  { value: "month", label: "MTD", accessible: "Month to date" },
  { value: "year", label: "YTD", accessible: "Year to date" },
];

function requestedRange(route: Route): BurnRateRange | undefined {
  const value = route.query?.range;
  return PULSE_RANGES.some((range) => range.value === value)
    ? value as BurnRateRange
    : undefined;
}

function periodScopeLabel(b: BurnRate): string {
  if (b.period === "day") return "Today · captured spend";
  const prefix = b.period === "week" ? "Week to date" : b.period === "month" ? "Month to date" : "Year to date";
  return `${prefix} · projected through ${shortDate(b.period_end)}`;
}

function pulseRangeSelector(b: BurnRate, route: Route): HTMLElement {
  const links = PULSE_RANGES.map((range) => el("a", {
    class: "pulse-range-option",
    href: routePath(route.segments, { ...(route.query ?? {}), range: range.value }),
    text: range.label,
    "aria-label": `Show ${range.accessible.toLowerCase()} spend`,
    "aria-current": b.period === range.value ? "true" : undefined,
  }));
  links.push(el("a", {
    class: "pulse-range-option pulse-range-custom",
    href: "#/investigate?mode=timeline",
    text: "Custom",
    "aria-label": "Choose a custom date range in Investigate",
  }));
  return el("nav", { class: "pulse-range-selector", "aria-label": "Forecast date range" }, links);
}

/// The Pulse spend-anatomy Beam. The estimated priced spend is partitioned into the
/// capped-potential RECOVERABLE slice and the REMAINDER — a single money lane (usd micros) whose two
/// segments sum to the scoped priced spend, so the lane total can never drift from the figures above.
/// Unpriced usage is held OUTSIDE the lane as a detached token-SHARE marker (`gap`), never a fabricated
/// dollar segment: honesty about coverage, not an invented number. Returns null when there is no priced
/// spend to partition (an empty lane would imply a total that isn't there).
export function pulseBeamModel(
  savings: SavingsLedger,
  unpricedSharePct: number
): BeamModel | null {
  const spend = Math.max(0, savings.total_spend_micros);
  if (spend <= 0) return null;
  // Capped potential is defined as ≤ spend; clamp defensively so RECOVERABLE never exceeds the lane
  // and REMAINDER never goes negative (both would corrupt the scoped-total invariant).
  const recoverableRaw = savings.capped_potential_micros ?? savings.total_recoverable_micros;
  const recoverable = Math.min(spend, Math.max(0, recoverableRaw));
  const remainder = spend - recoverable;
  const share = Math.max(0, Math.min(100, unpricedSharePct)) / 100;
  const model: BeamModel = {
    mode: "pulse",
    title: "Where the estimated spend sits",
    unit: "usd",
    segments: [
      { key: "recoverable", label: "Capped potential", value: recoverable, tone: "cost-warn" },
      { key: "remainder", label: "Remaining spend", value: remainder, tone: "cost-ok" },
    ],
  };
  // Only show the unpriced marker when there is unpriced usage — no marker means "everything priced".
  if (share > 0) model.gap = { label: "Unpriced usage", share };
  return model;
}

function nonnegativeFinite(value: number): number {
  return Number.isFinite(value) ? Math.max(0, value) : 0;
}

const usdWhole = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  maximumFractionDigits: 0,
});

function fmtUsdApprox(micros: number): string {
  const safe = nonnegativeFinite(micros);
  return safe < 1_000_000 ? fmtUsd(safe) : usdWhole.format(safe / 1_000_000);
}

function fmtAxisUsd(micros: number): string {
  const dollars = nonnegativeFinite(micros) / 1_000_000;
  if (dollars === 0) return "$0";
  if (dollars >= 1_000_000) return `$${Number((dollars / 1_000_000).toFixed(1))}m`;
  if (dollars >= 1_000) return `$${Number((dollars / 1_000).toFixed(1))}k`;
  if (dollars >= 1) return `$${Number(dollars.toFixed(1))}`;
  return fmtUsd(micros);
}

function escapeSvg(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function civilDateAt(start: string, offset: number): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(start);
  if (!match) return "";
  const date = new Date(Date.UTC(Number(match[1]), Number(match[2]) - 1, Number(match[3]) + offset));
  if (!Number.isFinite(date.getTime())) return "";
  return `${date.getUTCFullYear().toString().padStart(4, "0")}-${(date.getUTCMonth() + 1)
    .toString()
    .padStart(2, "0")}-${date.getUTCDate().toString().padStart(2, "0")}`;
}

function shortDate(iso: string): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
  if (!match) return iso;
  const month = MONTHS[Number(match[2]) - 1];
  return month ? `${month} ${Number(match[3])}` : iso;
}

function captureIsIncomplete(cov: Coverage): boolean {
  return cov.status !== "green" || cov.blind_sources.length > 0;
}

/// Round a money-domain ceiling to a readable 1/2/2.5/4/5/10 step. The chart keeps a true zero
/// baseline while avoiding arbitrary-looking top ticks such as "$3,323.22".
function niceMoneyCeiling(value: number): number {
  if (!(value > 0) || !Number.isFinite(value)) return 1;
  const magnitude = 10 ** Math.floor(Math.log10(value));
  const scaled = value / magnitude;
  const factor = scaled <= 1 ? 1 : scaled <= 2 ? 2 : scaled <= 2.5 ? 2.5 : scaled <= 4 ? 4 : scaled <= 5 ? 5 : 10;
  return factor * magnitude;
}

interface DirectLabel {
  key: string;
  text: string;
  targetY: number;
  stroke: string;
  fill: string;
}

function spreadLabelYs(labels: DirectLabel[], minY: number, maxY: number, gap: number): Map<string, number> {
  if (labels.length === 0) return new Map();
  const ordered = [...labels].sort((a, b) => a.targetY - b.targetY);
  const ys = ordered.map((label, index) => index === 0 ? Math.max(minY, label.targetY) : Math.max(label.targetY, minY));
  for (let i = 1; i < ys.length; i += 1) ys[i] = Math.max(ys[i], ys[i - 1] + gap);
  const overflow = ys[ys.length - 1] - maxY;
  if (overflow > 0) for (let i = 0; i < ys.length; i += 1) ys[i] -= overflow;
  for (let i = ys.length - 2; i >= 0; i -= 1) ys[i] = Math.min(ys[i], ys[i + 1] - gap);
  if (ys[0] < minY) {
    const shift = minY - ys[0];
    for (let i = 0; i < ys.length; i += 1) ys[i] += shift;
  }
  return new Map(ordered.map((label, index) => [label.key, ys[index]]));
}

/// Build a cumulative chart from the actual dense daily history returned by the projection API.
/// The solid step path is observed captured spend; dashed lines are deterministic pace projections;
/// the shaded fan is explicitly a low/high recent-pace scenario envelope, not a confidence interval.
type ForecastChartVariant = "ultrawide" | "wide" | "medium" | "compact";

function forecastLineSvgVariant(b: BurnRate, variant: ForecastChartVariant): string {
  const ultrawide = variant === "ultrawide";
  const compact = variant === "compact";
  const medium = variant === "medium";
  const W = ultrawide ? 1440 : compact ? 360 : medium ? 640 : 960;
  const H = ultrawide ? 300 : compact ? 270 : medium ? 250 : 254;
  const LEFT = ultrawide ? 72 : compact ? 48 : medium ? 60 : 64;
  const RIGHT = ultrawide ? 18 : compact ? 6 : 12;
  // Every size has a real label gutter. Overlaying endpoint labels on the plot at phone widths made
  // the fan unreadable precisely when the values clustered; abbreviated amounts keep this gutter
  // useful without turning it into a conventional detached legend.
  const LABEL_GUTTER = ultrawide ? 210 : compact ? 90 : medium ? 118 : 155;
  const PLOT_RIGHT = W - RIGHT - LABEL_GUTTER;
  const TOP = ultrawide ? 32 : compact ? 36 : 28;
  const BASE = ultrawide ? 226 : compact ? 218 : 194;
  const days = Math.max(1, nonnegativeFinite(b.days_in_period));
  const elapsed = Math.max(0, Math.min(nonnegativeFinite(b.days_elapsed), days));
  const remainingDays = Math.max(0, days - elapsed);
  const observedEnd = nonnegativeFinite(b.spent_micros);
  const projected = Math.max(observedEnd, nonnegativeFinite(b.projected_micros));
  const projectedLow = nonnegativeFinite(b.projected_low_micros);
  const projectedHigh = nonnegativeFinite(b.projected_high_micros);
  const bandLow = Math.max(observedEnd, Math.min(projectedLow, projectedHigh));
  const bandHigh = Math.max(bandLow, projectedLow, projectedHigh);
  const cap = nonnegativeFinite(b.cap_micros);
  const domainMax = Math.max(bandHigh, projected, cap, observedEnd, 1);
  const top = niceMoneyCeiling(domainMax * 1.04);
  const x = (d: number): number => LEFT + (d / days) * (PLOT_RIGHT - LEFT);
  const y = (v: number): number => BASE - (v / top) * (BASE - TOP);
  const pt = (d: number, v: number): string => `${x(d).toFixed(1)},${y(v).toFixed(1)}`;
  const currentX = x(elapsed);
  const currentY = y(observedEnd);
  const currentOnRight = elapsed / days > 0.72;
  const currentLabelX = currentX + (currentOnRight ? -7 : 7);
  const currentLabelY = currentY < TOP + 22 ? currentY + 20 : currentY - 9;
  const daily = Array.isArray(b.daily_spend_micros)
    ? b.daily_spend_micros.slice(0, elapsed).map(nonnegativeFinite)
    : [];
  const dailyTotal = daily.reduce((sum, value) => sum + value, 0);
  const historyIsComplete = daily.length === elapsed && dailyTotal === observedEnd;
  const startLabel = shortDate(b.period_start) || "Period start";
  const asOfLabel = shortDate(b.as_of) || `day ${Math.round(elapsed)}`;
  const endLabel = shortDate(b.period_end) || `day ${Math.round(days)}`;
  const rangeText = remainingDays > 0 && bandHigh > bandLow
    ? ` Recent-pace scenarios run from ${fmtUsd(bandLow)} to ${fmtUsd(bandHigh)}; this is not a probability interval.`
    : "";
  const capText = cap > 0 ? ` The configured cap is ${fmtUsd(cap)}.` : "";
  const description = remainingDays > 0
    ? `Captured spend from ${startLabel} through ${asOfLabel} is ${fmtUsd(observedEnd)}. The typical-pace period-end projection is ${fmtUsd(projected)}.${rangeText}${capText}`
    : `Captured spend for ${asOfLabel} is ${fmtUsd(observedEnd)}.${capText}`;
  const descId = `pulse-forecast-desc-${variant}`;

  const out: string[] = [
    `<svg class="pulse-chart-${variant}" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="100%" role="img" tabindex="0" focusable="true" aria-keyshortcuts="ArrowLeft ArrowRight Home End Escape" aria-label="Interactive cumulative captured-spend projection chart. Use Left and Right arrow keys to inspect daily values." aria-describedby="${descId}" data-chart-width="${W}" data-chart-height="${H}" data-plot-left="${LEFT}" data-plot-right="${PLOT_RIGHT}" data-plot-top="${TOP}" data-plot-base="${BASE}" data-domain-top="${top}" data-days="${days}" data-elapsed="${elapsed}">`,
    `<desc id="${descId}">${escapeSvg(description)}</desc>`,
    `<text class="pulse-chart-subtitle" x="${LEFT}" y="14" font-size="11" font-weight="600" fill="var(--muted)">Cumulative captured spend · USD</text>`,
  ];
  const ticks = compact ? [top, top / 2, 0] : [top, top * 0.75, top / 2, top * 0.25, 0];
  for (const [index, tick] of ticks.entries()) {
    const tickY = y(tick);
    out.push(
      `<line class="pulse-gridline" x1="${LEFT}" y1="${tickY.toFixed(1)}" x2="${PLOT_RIGHT}" y2="${tickY.toFixed(1)}" stroke="var(--border-strong)" stroke-opacity="${index === ticks.length - 1 ? "0.52" : "0.22"}"/>`
    );
    out.push(
      `<text class="pulse-y-tick" x="${LEFT - 8}" y="${(tickY + 3.5).toFixed(1)}" text-anchor="end" font-size="10.5" fill="var(--muted)">${fmtAxisUsd(tick)}</text>`
    );
  }
  if (cap > 0) {
    out.push(
      `<line class="pulse-cap" x1="${x(0).toFixed(1)}" y1="${y(0).toFixed(1)}" x2="${x(days).toFixed(1)}" y2="${y(cap).toFixed(1)}" stroke="var(--border-strong)" stroke-width="1.75" stroke-dasharray="2 3"/>`
    );
  }
  out.push(
    `<line class="pulse-today" x1="${currentX.toFixed(1)}" y1="${TOP}" x2="${currentX.toFixed(1)}" y2="${BASE}" stroke="var(--border-strong)" stroke-opacity="0.7" stroke-dasharray="2 4"/>`
  );
  out.push(
    `<text class="pulse-today-label" x="${(currentX + (currentOnRight ? -6 : 6)).toFixed(1)}" y="${TOP + 13}" text-anchor="${currentOnRight ? "end" : "start"}" font-size="10.5" fill="var(--muted)">Today · ${escapeSvg(asOfLabel)}</text>`
  );
  if (remainingDays > 0 && bandHigh > bandLow) {
    out.push(
      `<polygon class="pulse-band" points="${pt(elapsed, observedEnd)} ${pt(days, bandHigh)} ${pt(days, bandLow)}" fill="var(--data-ink)" fill-opacity="0.18"/>`,
      `<line class="pulse-band-edge" x1="${currentX.toFixed(1)}" y1="${currentY.toFixed(1)}" x2="${x(days).toFixed(1)}" y2="${y(bandHigh).toFixed(1)}" stroke="var(--data-ink)" stroke-opacity="0.72" stroke-width="1.5"/>`,
      `<line class="pulse-band-edge" x1="${currentX.toFixed(1)}" y1="${currentY.toFixed(1)}" x2="${x(days).toFixed(1)}" y2="${y(bandLow).toFixed(1)}" stroke="var(--data-ink)" stroke-opacity="0.72" stroke-width="1.5"/>`
    );
  }
  if (historyIsComplete && elapsed > 0) {
    let cumulative = 0;
    const path = [`M ${pt(0, 0)}`];
    daily.forEach((value, index) => {
      path.push(`H ${x(index + 1).toFixed(1)}`);
      cumulative += value;
      path.push(`V ${y(cumulative).toFixed(1)}`);
    });
    out.push(
      `<path class="pulse-observed" d="${path.join(" ")}" fill="none" stroke="var(--data-ink)" stroke-width="2.75" stroke-linecap="round" stroke-linejoin="round"/>`
    );
  } else if (elapsed > 0) {
    out.push(
      `<text class="pulse-history-missing" x="${LEFT + 8}" y="${BASE - 10}" font-size="10.5" fill="var(--muted)">Daily history unavailable</text>`
    );
  }
  if (remainingDays > 0) {
    out.push(
      `<line class="pulse-forecast" x1="${currentX.toFixed(1)}" y1="${currentY.toFixed(1)}" x2="${x(days).toFixed(1)}" y2="${y(projected).toFixed(1)}" stroke="var(--data-ink)" stroke-opacity="0.82" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="5 4"/>`
    );
  }
  out.push(
    `<circle class="pulse-current-point" cx="${currentX.toFixed(1)}" cy="${currentY.toFixed(1)}" r="4" fill="var(--data-ink)"/>`
  );
  if (remainingDays > 0) {
    out.push(
      `<circle class="pulse-forecast-point" cx="${x(days).toFixed(1)}" cy="${y(projected).toFixed(1)}" r="4" fill="var(--bg)" stroke="var(--data-ink)" stroke-width="2"/>`
    );
  }
  out.push(
    `<text class="pulse-current-value pulse-direct-label" x="${currentLabelX.toFixed(1)}" y="${currentLabelY.toFixed(1)}" text-anchor="${currentOnRight ? "end" : "start"}" font-size="10.5" font-weight="600" fill="var(--text)">Captured ${escapeSvg(fmtUsdApprox(observedEnd))}</text>`
  );
  const endpointLabels: DirectLabel[] = remainingDays > 0 ? [
    {
      key: "typical",
      text: compact ? `Typical ${fmtAxisUsd(projected)}` : `Typical pace · ${fmtUsdApprox(projected)}`,
      targetY: y(projected),
      stroke: "var(--data-ink)",
      fill: "var(--text)",
    },
  ] : [];
  if (remainingDays > 0 && bandHigh > bandLow) {
    endpointLabels.push(
      { key: "high", text: compact ? `High ${fmtAxisUsd(bandHigh)}` : `High pace · ${fmtUsdApprox(bandHigh)}`, targetY: y(bandHigh), stroke: "var(--data-ink)", fill: "var(--text)" },
      { key: "low", text: compact ? `Low ${fmtAxisUsd(bandLow)}` : `Low pace · ${fmtUsdApprox(bandLow)}`, targetY: y(bandLow), stroke: "var(--data-ink)", fill: "var(--text)" }
    );
  }
  if (cap > 0) {
    endpointLabels.push({ key: "cap", text: `Cap · ${compact ? fmtAxisUsd(cap) : fmtUsdApprox(cap)}`, targetY: y(cap), stroke: "var(--border-strong)", fill: "var(--muted)" });
  }
  const labelYs = spreadLabelYs(endpointLabels, TOP + 11, BASE - 5, compact ? 15 : 14);
  const endpointTextX = PLOT_RIGHT + (compact ? 7 : 10);
  const endpointTextAnchor = "start";
  const endpointLeaderX = PLOT_RIGHT + (compact ? 5 : 7);
  for (const label of endpointLabels) {
    const labelY = labelYs.get(label.key) ?? label.targetY;
    out.push(
      `<line class="pulse-label-leader" x1="${PLOT_RIGHT.toFixed(1)}" y1="${label.targetY.toFixed(1)}" x2="${endpointLeaderX.toFixed(1)}" y2="${(labelY - 3).toFixed(1)}" stroke="${label.stroke}" stroke-width="1"/>`,
      `<text class="pulse-direct-label pulse-label-${label.key}" x="${endpointTextX.toFixed(1)}" y="${labelY.toFixed(1)}" text-anchor="${endpointTextAnchor}" font-size="${compact ? "9.5" : "10.5"}" font-weight="600" fill="${label.fill}">${escapeSvg(label.text)}</text>`
    );
  }
  out.push(`<text x="${LEFT}" y="${H - 10}" font-size="10.5" fill="var(--muted)">${escapeSvg(startLabel)}</text>`);
  out.push(
    `<text x="${PLOT_RIGHT}" y="${H - 10}" text-anchor="end" font-size="10.5" fill="var(--muted)">${escapeSvg(endLabel)}</text>`
  );
  out.push(
    `<rect class="pulse-chart-hit-target" x="${LEFT}" y="${TOP}" width="${(PLOT_RIGHT - LEFT).toFixed(1)}" height="${(BASE - TOP).toFixed(1)}" fill="transparent"/>`,
    `<g class="pulse-inspector" aria-hidden="true">`,
    `<line class="pulse-inspector-line" x1="${LEFT}" y1="${TOP}" x2="${LEFT}" y2="${BASE}"/>`,
    `<circle class="pulse-inspector-point series-captured" data-inspector-series="captured" r="4"/>`,
    `<circle class="pulse-inspector-point series-typical" data-inspector-series="typical" r="4"/>`,
    `<circle class="pulse-inspector-point series-low" data-inspector-series="low" r="3.5"/>`,
    `<circle class="pulse-inspector-point series-high" data-inspector-series="high" r="3.5"/>`,
    `<circle class="pulse-inspector-point series-cap" data-inspector-series="cap" r="3.5"/>`,
    `</g>`
  );
  out.push(`</svg>`);
  return out.join("");
}

function forecastLineSvg(b: BurnRate): string {
  return forecastLineSvgVariant(b, "ultrawide") + forecastLineSvgVariant(b, "wide") + forecastLineSvgVariant(b, "medium") + forecastLineSvgVariant(b, "compact");
}

type InspectorSeries = "captured" | "typical" | "low" | "high" | "cap";

function longDate(iso: string): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
  if (!match) return iso;
  const month = MONTHS[Number(match[2]) - 1];
  return month ? `${month} ${Number(match[3])}, ${match[1]}` : iso;
}

/// Add Datadog-style inspection without baking transient UI into the SVG renderer: pointer/touch
/// snaps to a civil day, while the same model is available with arrows/Home/End from the keyboard.
/// The HTML tooltip can reflow and remain legible on phones; the SVG group only draws the crosshair.
function wireForecastInteraction(host: HTMLElement, b: BurnRate): void {
  const tooltip = el("div", {
    id: "pulse-chart-tooltip",
    class: "pulse-chart-tooltip",
    role: "status",
    "aria-live": "polite",
    "aria-atomic": "true",
  });
  tooltip.hidden = true;
  host.appendChild(tooltip);

  const svgs = Array.from(host.querySelectorAll<SVGSVGElement>("svg"));
  const selected = new WeakMap<SVGSVGElement, number>();
  const elapsed = Math.max(0, Math.min(Math.round(nonnegativeFinite(b.days_elapsed)), Math.round(nonnegativeFinite(b.days_in_period))));
  const days = Math.max(1, Math.round(nonnegativeFinite(b.days_in_period)));
  const spent = nonnegativeFinite(b.spent_micros);
  const projected = Math.max(spent, nonnegativeFinite(b.projected_micros));
  const projectedLow = nonnegativeFinite(b.projected_low_micros);
  const projectedHigh = nonnegativeFinite(b.projected_high_micros);
  const lowEnd = Math.max(spent, Math.min(projectedLow, projectedHigh));
  const highEnd = Math.max(lowEnd, projectedLow, projectedHigh);
  const daily = Array.isArray(b.daily_spend_micros)
    ? b.daily_spend_micros.slice(0, elapsed).map(nonnegativeFinite)
    : [];
  let running = 0;
  const cumulative = daily.map((value) => {
    running += value;
    return running;
  });
  const historyIsComplete = daily.length === elapsed && running === spent;

  const valuesAt = (index: number): Partial<Record<InspectorSeries, number>> => {
    const values: Partial<Record<InspectorSeries, number>> = {};
    if (index <= elapsed) {
      if (historyIsComplete) values.captured = cumulative[index - 1] ?? 0;
      else if (index === elapsed) values.captured = spent;
    } else if (elapsed < days) {
      const progress = (index - elapsed) / (days - elapsed);
      values.typical = Math.round(spent + (projected - spent) * progress);
      if (highEnd > lowEnd) {
        values.low = Math.round(spent + (lowEnd - spent) * progress);
        values.high = Math.round(spent + (highEnd - spent) * progress);
      }
    }
    if (b.cap_micros > 0) values.cap = Math.round(nonnegativeFinite(b.cap_micros) * index / days);
    return values;
  };

  const tooltipRow = (series: InspectorSeries, label: string, value: number): HTMLElement =>
    el("div", { class: `pulse-tooltip-row series-${series}` }, [
      el("dt", {}, [
        el("span", { class: "pulse-tooltip-swatch", "aria-hidden": "true" }),
        label,
      ]),
      el("dd", { class: "num", text: fmtUsd(value) }),
    ]);

  const hide = (): void => {
    for (const svg of svgs) {
      svg.querySelector(".pulse-inspector")?.classList.remove("is-active");
    }
    tooltip.hidden = true;
  };

  const show = (svg: SVGSVGElement, requestedIndex: number): void => {
    const index = Math.max(1, Math.min(days, Math.round(requestedIndex)));
    selected.set(svg, index);
    for (const candidate of svgs) {
      candidate.querySelector(".pulse-inspector")?.classList.toggle("is-active", candidate === svg);
    }

    const chartWidth = Number(svg.dataset.chartWidth) || 1;
    const left = Number(svg.dataset.plotLeft) || 0;
    const right = Number(svg.dataset.plotRight) || chartWidth;
    const plotTop = Number(svg.dataset.plotTop) || 0;
    const plotBase = Number(svg.dataset.plotBase) || 1;
    const domainTop = Number(svg.dataset.domainTop) || 1;
    const x = left + (index / days) * (right - left);
    const y = (value: number): number => plotBase - (value / domainTop) * (plotBase - plotTop);
    const group = svg.querySelector<SVGGElement>(".pulse-inspector");
    const line = group?.querySelector<SVGLineElement>(".pulse-inspector-line");
    if (line) {
      line.setAttribute("x1", x.toFixed(1));
      line.setAttribute("x2", x.toFixed(1));
    }
    const values = valuesAt(index);
    for (const series of ["captured", "typical", "low", "high", "cap"] as const) {
      const point = group?.querySelector<SVGCircleElement>(`[data-inspector-series="${series}"]`);
      const value = values[series];
      if (!point || value === undefined) {
        point?.setAttribute("visibility", "hidden");
        continue;
      }
      point.setAttribute("cx", x.toFixed(1));
      point.setAttribute("cy", y(value).toFixed(1));
      point.setAttribute("visibility", "visible");
    }

    const date = civilDateAt(b.period_start, index - 1);
    const actual = index <= elapsed;
    const rows: HTMLElement[] = [];
    if (values.captured !== undefined) {
      rows.push(tooltipRow("captured", "Captured cumulative", values.captured));
      if (historyIsComplete) rows.push(tooltipRow("captured", "Spend that day", daily[index - 1] ?? 0));
    } else if (actual) {
      rows.push(el("div", { class: "pulse-tooltip-unavailable", text: "Daily captured history unavailable" }));
    }
    if (values.typical !== undefined) rows.push(tooltipRow("typical", "Typical pace", values.typical));
    if (values.low !== undefined) rows.push(tooltipRow("low", "Low pace", values.low));
    if (values.high !== undefined) rows.push(tooltipRow("high", "High pace", values.high));
    if (values.cap !== undefined) rows.push(tooltipRow("cap", "Cap pace", values.cap));
    tooltip.replaceChildren(
      el("p", { class: "pulse-tooltip-date", text: longDate(date) }),
      el("p", {
        class: "pulse-tooltip-context",
        text: `${actual ? "Captured" : "Projected"} · day ${index} of ${days}`,
      }),
      el("dl", { class: "pulse-tooltip-values" }, rows)
    );
    tooltip.hidden = false;

    const svgRect = svg.getBoundingClientRect();
    const hostRect = host.getBoundingClientRect();
    const renderedX = svgRect.width > 0
      ? svgRect.left - hostRect.left + (x / chartWidth) * svgRect.width
      : x;
    const renderedTop = svgRect.height > 0
      ? svgRect.top - hostRect.top + (plotTop / (Number(svg.dataset.chartHeight) || 1)) * svgRect.height
      : plotTop;
    tooltip.style.left = `${renderedX}px`;
    tooltip.style.top = `${Math.max(8, renderedTop + 8)}px`;
    tooltip.dataset.align = (x - left) / Math.max(1, right - left) > 0.62 ? "right" : "left";
  };

  for (const svg of svgs) {
    svg.setAttribute("aria-controls", tooltip.id);
    const indexFromPointer = (event: PointerEvent): number => {
      const rect = svg.getBoundingClientRect();
      if (!(rect.width > 0)) return selected.get(svg) ?? Math.max(1, elapsed);
      const chartWidth = Number(svg.dataset.chartWidth) || 1;
      const left = Number(svg.dataset.plotLeft) || 0;
      const right = Number(svg.dataset.plotRight) || chartWidth;
      const chartX = (event.clientX - rect.left) / rect.width * chartWidth;
      return Math.round((chartX - left) / Math.max(1, right - left) * days);
    };
    svg.addEventListener("pointermove", (event) => {
      if ((event as PointerEvent).pointerType === "touch") return;
      show(svg, indexFromPointer(event as PointerEvent));
    });
    svg.addEventListener("pointerdown", (event) => {
      show(svg, indexFromPointer(event as PointerEvent));
      svg.focus({ preventScroll: true });
    });
    svg.addEventListener("pointerleave", (event) => {
      if ((event as PointerEvent).pointerType !== "touch" && document.activeElement !== svg) hide();
    });
    svg.addEventListener("focus", () => show(svg, selected.get(svg) ?? Math.max(1, elapsed)));
    svg.addEventListener("blur", hide);
    svg.addEventListener("keydown", (event) => {
      const keyboard = event as KeyboardEvent;
      const current = selected.get(svg) ?? Math.max(1, elapsed);
      let next: number | undefined;
      if (keyboard.key === "ArrowLeft") next = current - 1;
      else if (keyboard.key === "ArrowRight") next = current + 1;
      else if (keyboard.key === "Home") next = 1;
      else if (keyboard.key === "End") next = days;
      else if (keyboard.key === "Escape") {
        keyboard.preventDefault();
        hide();
        return;
      }
      if (next !== undefined) {
        keyboard.preventDefault();
        show(svg, next);
      }
    });
  }
}

interface ForecastDataDisclosure {
  toggle: HTMLButtonElement;
  panel: HTMLElement;
}

function forecastDataDisclosure(b: BurnRate): ForecastDataDisclosure {
  const daily = Array.isArray(b.daily_spend_micros) ? b.daily_spend_micros.map(nonnegativeFinite) : [];
  let cumulative = 0;
  const rows = daily.map((value, index) => {
    cumulative += value;
    return el("tr", {}, [
      el("th", { scope: "row", text: shortDate(civilDateAt(b.period_start, index)) || `Day ${index + 1}` }),
      el("td", { text: "Captured" }),
      el("td", { class: "num", text: fmtUsd(value) }),
      el("td", { class: "num", text: fmtUsd(cumulative) }),
    ]);
  });
  const periodEnd = shortDate(b.period_end) || "Period end";
  const hasFuture = nonnegativeFinite(b.days_elapsed) < nonnegativeFinite(b.days_in_period);
  const bandLow = Math.max(nonnegativeFinite(b.spent_micros), Math.min(nonnegativeFinite(b.projected_low_micros), nonnegativeFinite(b.projected_high_micros)));
  const bandHigh = Math.max(bandLow, nonnegativeFinite(b.projected_low_micros), nonnegativeFinite(b.projected_high_micros));
  if (hasFuture && bandHigh > bandLow) {
    rows.push(el("tr", {}, [el("th", { scope: "row", text: periodEnd }), el("td", { text: "Low recent pace" }), el("td", { text: "—" }), el("td", { class: "num", text: fmtUsd(bandLow) })]));
  }
  if (hasFuture) {
    rows.push(el("tr", {}, [el("th", { scope: "row", text: periodEnd }), el("td", { text: "Typical pace" }), el("td", { text: "—" }), el("td", { class: "num", text: fmtUsd(Math.max(nonnegativeFinite(b.spent_micros), nonnegativeFinite(b.projected_micros))) })]));
  }
  if (hasFuture && bandHigh > bandLow) {
    rows.push(el("tr", {}, [el("th", { scope: "row", text: periodEnd }), el("td", { text: "High recent pace" }), el("td", { text: "—" }), el("td", { class: "num", text: fmtUsd(bandHigh) })]));
  }
  if (b.cap_micros > 0) {
    rows.push(el("tr", {}, [el("th", { scope: "row", text: periodEnd }), el("td", { text: "Configured cap" }), el("td", { text: "—" }), el("td", { class: "num", text: fmtUsd(b.cap_micros) })]));
  }
  const panelId = "pulse-chart-data-panel";
  const panel = el("div", { id: panelId, class: "pulse-chart-data-panel" }, [
    el("p", {
      class: "caption sub",
      text: "Low and high are deterministic scenarios using the 25th and 75th percentiles of non-zero captured days. They are not a confidence interval.",
    }),
    el("div", { class: "pulse-chart-table-wrap" }, [
      el("table", { class: "data pulse-chart-table" }, [
        el("caption", { class: "sr-only", text: "Daily captured spend and period-end pace projections" }),
        el("thead", {}, [el("tr", {}, [el("th", { scope: "col", text: "Date" }), el("th", { scope: "col", text: "Series" }), el("th", { scope: "col", text: "Daily" }), el("th", { scope: "col", text: "Cumulative or period end" })])]),
        el("tbody", {}, rows),
      ]),
    ]),
  ]);
  panel.hidden = true;
  const toggle = el("button", {
    class: "pulse-chart-data-toggle",
    type: "button",
    "aria-expanded": "false",
    "aria-controls": panelId,
    "aria-label": "View daily data and projection methodology",
  }, [icon("chevron", { size: 12 }), el("span", { text: "Data & methodology" })]) as HTMLButtonElement;
  toggle.addEventListener("click", () => {
    const expanded = toggle.getAttribute("aria-expanded") !== "true";
    toggle.setAttribute("aria-expanded", String(expanded));
    panel.hidden = !expanded;
  });
  return { toggle, panel };
}

function forecastChart(b: BurnRate, cov: Coverage, route: Route): HTMLElement {
  const wrapper = el("div", { class: "pulse-forecast" });
  const chartHost = rawSvg(el("div", { class: "pulse-forecast-line chart-host" }), forecastLineSvg(b));
  wireForecastInteraction(chartHost, b);
  wrapper.appendChild(chartHost);
  const data = forecastDataDisclosure(b);
  wrapper.appendChild(el("div", { class: "pulse-chart-footer" }, [
    el("div", { class: "pulse-chart-footer-bar" }, [data.toggle, trustStrip(cov, route)]),
    data.panel,
  ]));
  return wrapper;
}

/// The one scoped spend answer + its period delta. The answer is the projected
/// end-of-period spend; the delta is measured against the cap pace (the available comparable baseline),
/// stated honestly with the on-track verdict. Never a fabricated certainty — projection + band only.
function spendAnswer(b: BurnRate, cov: Coverage, route: Route): HTMLElement {
  const period = periodLabel(b.period);
  const isDay = b.period === "day";
  const cap = Math.max(0, b.cap_micros);
  const captured = nonnegativeFinite(b.spent_micros);
  const projected = Math.max(captured, nonnegativeFinite(b.projected_micros));
  const low = Math.max(captured, Math.min(nonnegativeFinite(b.projected_low_micros), nonnegativeFinite(b.projected_high_micros)));
  const high = Math.max(low, nonnegativeFinite(b.projected_low_micros), nonnegativeFinite(b.projected_high_micros));
  const incompleteCapture = captureIsIncomplete(cov);
  const remaining = Math.max(0, Math.round(nonnegativeFinite(b.days_in_period) - nonnegativeFinite(b.days_elapsed)));
  const observedOnly = isDay || remaining === 0;
  const headline = observedOnly ? captured : projected;
  const answer = el("div", { class: "pulse-answer" }, [
    el("p", { class: "pulse-answer-label", text: observedOnly ? `Captured spend ${period}` : `Projected captured spend ${period}` }),
    el("p", {
      class: "stat display num",
      title: observedOnly ? `Exact captured spend: ${fmtUsd(headline)}` : `Exact typical-pace projection: ${fmtUsd(headline)}`,
      text: fmtUsdApprox(headline),
    }),
  ]);
  if (!observedOnly && high > low) {
    answer.appendChild(
      el("p", {
        class: "pulse-band-label caption sub",
        title: "Low/high use the 25th/75th percentile of non-zero captured days; this is not a confidence interval.",
        text: `Recent-pace scenarios ${fmtUsdApprox(low)}–${fmtUsdApprox(high)} · ${remaining} ${remaining === 1 ? "day" : "days"} remaining`,
      })
    );
  } else if (!observedOnly && b.active_days < 2) {
    answer.appendChild(el("p", { class: "pulse-band-label caption sub", text: "More active days are needed to show a pace range." }));
  }
  if (cap > 0) {
    let tone = "cost-ok";
    let label = "";
    let warningIcon = false;
    if (incompleteCapture) {
      const delta = projected - cap;
      tone = "muted";
      label = `Typical-pace projection is ${fmtUsdApprox(Math.abs(delta))} ${delta > 0 ? "over" : "under"} the ${fmtUsdApprox(cap)} cap · based on captured data`;
    } else if (high > low && high <= cap) {
      label = high === cap
        ? `Recent-pace scenarios stay within the ${fmtUsdApprox(cap)} cap`
        : `Recent-pace scenarios stay ${fmtUsdApprox(cap - high)} under the ${fmtUsdApprox(cap)} cap`;
    } else if (high > low && low > cap) {
      tone = "cost-high";
      warningIcon = true;
      label = `Recent-pace scenarios exceed the ${fmtUsdApprox(cap)} cap by at least ${fmtUsdApprox(low - cap)}`;
    } else if (high > low) {
      tone = "cost-warn";
      warningIcon = true;
      label = `At risk — recent-pace scenarios cross the ${fmtUsdApprox(cap)} cap`;
    } else {
      const delta = projected - cap;
      tone = delta <= 0 ? "cost-ok" : "cost-high";
      warningIcon = delta > 0;
      label = `Typical-pace projection is ${fmtUsdApprox(Math.abs(delta))} ${delta > 0 ? "over" : "under"} the ${fmtUsdApprox(cap)} cap`;
    }
    answer.appendChild(
      el("p", { class: `pulse-delta ${tone}` }, [
        icon(warningIcon ? "warning" : "dot", { size: 12, label: warningIcon ? "Warning" : undefined }),
        el("span", { text: ` ${label}` }),
      ])
    );
  } else {
    // A one-day range needs no rate-derived supporting statistic, and day/year are not valid
    // configured budget periods. Avoid offering a misleading monthly cap on either range.
    if (!isDay) {
      const items = [
        el("div", { class: "pulse-support-item" }, [
          el("dt", { text: "Typical active day" }),
          el("dd", { class: "num", text: b.active_days > 0 ? fmtUsdApprox(b.run_rate_micros_per_day) : "—" }),
        ]),
        el("div", { class: "pulse-support-item" }, [
          el("dt", { text: "Active days" }),
          el("dd", {
            class: "num",
            text: `${b.active_days} / ${b.days_elapsed}`,
            title: `${b.active_days} active days out of ${b.days_elapsed} elapsed days`,
          }),
        ]),
      ];
      if (b.period === "week" || b.period === "month") {
        const capPeriod = b.period === "week" ? "Weekly" : "Monthly";
        items.push(el("div", { class: "pulse-support-item" }, [
          el("dt", { text: `${capPeriod} cap` }),
          el("dd", { class: "pulse-cap-setup" }, [
            el("span", { class: "muted", text: "Not set" }),
            el("a", {
              class: "pulse-set-cap",
              href: routePath(route.segments, { ...(route.query ?? {}), settings: "data", sheet: "settings" }),
              text: "Set cap",
              "aria-label": `Set ${capPeriod.toLowerCase()} cap`,
            }),
          ]),
        ]));
      }
      answer.appendChild(
        el("dl", {
          class: `pulse-support pulse-support-${items.length}`,
          title: `Projection uses an activity-adjusted calendar rate of ${fmtUsd(b.effective_rate_micros_per_day)} per day.`,
        }, items)
      );
    }
  }
  return answer;
}

/// Compact trust strip: capture health as an HONEST status (never a fabricated %), the
/// active channels, and any blind-source warning — the same coverage semantics the Overview uses, kept
/// distinct from the analytical figures.
function trustStrip(cov: Coverage, route: Route): HTMLElement {
  const tone =
    cov.status === "green" ? "cost-ok" : cov.status === "red" ? "cost-high" : cov.status === "amber" ? "cost-warn" : "muted";
  const channels =
    [cov.has_proxy ? "proxy" : null, cov.has_otel ? "OTel" : null].filter(Boolean).join(" + ") || "none";
  const label =
    cov.status === "green"
      ? "Capture healthy"
      : cov.status === "red"
        ? "Capture unavailable"
        : cov.status === "amber"
          ? channels === "none" ? "Capture needs review" : `${channels} capture`
          : "Capture not set";
  const dotTone = cov.status === "green" ? "ok" : cov.status === "red" ? "error" : "warn";
  return el("div", { class: "pulse-trust", title: `Capture status: ${cov.status}; channels: ${channels}` }, [
    el("span", { class: "pulse-trust-status caption" }, [
      icon("dot", { size: 8, class: `capture-dot capture-${dotTone}` }),
      el("span", { class: tone, text: ` ${label}` }),
    ]),
    // Blind sources already become a concrete Needs attention row. Repeating the warning beside the
    // chart made a secondary provenance detail look like a second forecast verdict.
    el("a", {
      class: "pulse-trust-details sub",
      href: routePath(route.segments, { ...(route.query ?? {}), sheet: "trust" }),
      text: "Coverage",
      "aria-label": "Review capture coverage and pricing",
    }),
  ]);
}

// ---- Attention queue ------------------------------------------------

/// A single dollar-ranked attention row: a plain-language sentence + its dollar impact + a deep-link to
/// the evidence that explains it. `dollars` drives the ranking; capture gaps carry no dollar figure
/// (honest — that spend is uncaptured, not zero) so they sort last via a sentinel but still surface.
export interface AttnRow {
  kind: string;
  dollars: number; // impact for ranking; -1 for a no-dollar capture gap (ranks last, still shown)
  sentence: string;
  href: string;
  anomaly?: { date: string; seriesKey: string };
}

interface AttentionSources {
  anomalies: Anomaly[];
  budget: PeriodBudget;
  failures: FailureWasteReport;
  loops: LoopWasteReport;
  coverage: Coverage;
}

/// Normalize anomalies / budget / failures / loops / incomplete-capture into ONE dollar-ranked list of
/// sentence rows. Pure + deterministic so the ranking + wording are unit-testable. Each row
/// becomes a scope-preserving Selection A when the shared workspace context is present.
export function buildAttentionRows(s: AttentionSources): AttnRow[] {
  const rows: AttnRow[] = [];
  // Anomalies — ranked by the dollar delta from their own baseline.
  for (const a of s.anomalies) {
    const delta = a.value_micros - a.baseline_micros;
    if (delta <= 0) continue; // only spend that ROSE deserves attention here
    rows.push({
      kind: "anomaly",
      dollars: delta,
      sentence: `Spend on ${a.series_key} rose ${fmtUsd(delta)} above its baseline on ${a.date}.`,
      href: routePath(["investigate"], { mode: "timeline" }),
      anomaly: { date: a.date, seriesKey: a.series_key },
    });
  }
  // Budget — only when at/over the warn threshold (never a nag when on track).
  if (s.budget.status === "over" || s.budget.status === "warn") {
    const over = s.budget.spent_micros - s.budget.cap_micros;
    rows.push({
      kind: "budget",
      dollars: Math.max(0, over),
      sentence:
        s.budget.status === "over"
          ? `Over the ${s.budget.period} cap by ${fmtUsd(over)} (${fmtUsd(s.budget.spent_micros)} of ${fmtUsd(s.budget.cap_micros)}).`
          : `Approaching the ${s.budget.period} cap — ${s.budget.pct}% of ${fmtUsd(s.budget.cap_micros)} used.`,
      href: routePath(["pulse"], { sheet: "settings", settings: "data" }),
    });
  }
  // Failed steps — wasted spend on failures.
  if (s.failures.total_micros > 0) {
    rows.push({
      kind: "failure",
      dollars: s.failures.total_micros,
      sentence: `${fmtUsd(s.failures.total_micros)} spent on ${s.failures.total_failed_steps} failed step(s) (${s.failures.pct_of_spend}% of spend).`,
      href: routePath(["optimize"]),
    });
  }
  // Looping / redundant steps.
  if (s.loops.total_micros > 0) {
    rows.push({
      kind: "loop",
      dollars: s.loops.total_micros,
      sentence: `${fmtUsd(s.loops.total_micros)} on ${s.loops.total_redundant_steps} redundant/looping step(s).`,
      href: routePath(["optimize"]),
    });
  }
  // Incomplete capture — heartbeats seen but no cost steps; that spend is UNCAPTURED, never $0 (honest).
  if (s.coverage.blind_sources.length > 0) {
    rows.push({
      kind: "capture",
      dollars: -1, // no dollar figure — uncaptured spend, ranks last but is never hidden
      sentence: `Capture is blind to ${s.coverage.blind_sources.join(", ")} — that spend is missing from the totals, not zero.`,
      href: routePath(["pulse"], { sheet: "capture" }),
    });
  }
  // Rank by dollar impact desc; the no-dollar capture gap (−1) sorts to the end.
  return rows.sort((x, y) => y.dollars - x.dollars);
}

const FILTERABLE_SERIES_DIMENSIONS = new Set<CohortDimension>([
  "step",
  "component",
  "parent",
  "tool",
  "agent",
  "effort",
  "model",
  "provider",
  "session",
  "mcp_server",
  "commit",
  "author",
  "template",
  "source",
  "workload_key",
  "cache_class",
  "ttl",
  "stop_reason",
  "run_date",
]);

function anomalySeriesFilter(seriesKey: string): CohortFilter | null {
  const split = seriesKey.indexOf(":");
  // Pulse requests model-grouped anomalies, whose real wire key is the bare model name. Keep the
  // explicit `dimension:value` form too for deterministic fixtures and future grouped feeds.
  if (split < 0) {
    return seriesKey === "total" ? null : { op: "eq", dimension: "model", value: seriesKey };
  }
  if (split === 0 || split === seriesKey.length - 1) return null;
  const dimension = seriesKey.slice(0, split) as CohortDimension;
  const value = seriesKey.slice(split + 1);
  return FILTERABLE_SERIES_DIMENSIONS.has(dimension)
    ? { op: "eq", dimension, value }
    : null;
}

/// Convert an attention item into Selection A. A dated anomaly narrows the current scope to its day
/// and, when the series key names a cohort dimension (`model:gpt-4o`, `provider:openai`, …), adds
/// that exact equality filter. Aggregate budget/failure/loop/capture rows honestly select the active
/// scope because their legacy aggregate APIs do not expose a narrower resolvable entity set.
export function attentionSelection(row: AttnRow, scope: CohortSpec): CohortSpec {
  const filter = row.anomaly ? anomalySeriesFilter(row.anomaly.seriesKey) : null;
  return {
    ...scope,
    from: row.anomaly?.date ?? scope.from ?? null,
    to: row.anomaly?.date ?? scope.to ?? null,
    filters: filter ? [...scope.filters, filter] : [...scope.filters],
  };
}

/// Use the v2 detector-owned cohort snapshot for a controllable driver. It is the exact, resolvable
/// affected cohort even when the inline evidence/run list is capped; falling back to active scope is
/// honest for a compatibility v1 opportunity that has no v2 evidence yet.
export function driverSelection(
  match: NonNullable<SavingsLedger["opportunities_v2"]>[number] | undefined,
  scope: CohortSpec
): CohortSpec {
  const selected = match?.cohort_snapshot ?? scope;
  return { ...selected, filters: [...selected.filters] };
}

function selectionLink(
  selection: CohortSpec,
  fallbackHref: string,
  client: TareClient,
  context: WorkspaceContext | undefined,
  onSaveError: (error: unknown, retry: () => void) => void
): { href: string; onClick?: (event: Event) => void; title?: string } {
  if (!context) return { href: fallbackHref };
  const destination = parseHash(fallbackHref);
  const workspace: "investigate" | "optimize" =
    destination.name === "optimize" ? "optimize" : "investigate";
  const nextState = { ...context.analysis.get(), workspace, selection };
  const serialized = serializeAnalysisHash(
    destination.segments,
    nextState,
    undefined,
    destination.query
  );
  if (serialized.kind === "requires_save") {
    const id = `pulse-${cohortHash(selection)}-${Date.now().toString(36)}-${(++savedSelectionSequence).toString(36)}`;
    const savedHash = serializeAnalysisHash(destination.segments, nextState, id, destination.query);
    if (savedHash.kind !== "inline") return { href: fallbackHref };
    let saving = false;
    const save = (link?: HTMLElement | null): void => {
      if (saving) return;
      saving = true;
      link?.setAttribute("aria-busy", "true");
      const now = new Date().toISOString();
      const investigation = investigationFromState(id, "Pulse Selection A", nextState, now);
      void client
        .saveInvestigation(investigation)
        .then(() => {
          context.analysis.setSelection(selection);
          context.analysis.navigateWorkspace(workspace);
          window.location.hash = savedHash.hash;
        })
        .catch((error) => onSaveError(error, () => save()))
        .finally(() => {
          saving = false;
          link?.removeAttribute("aria-busy");
        });
    };
    return {
      // Do not expose a not-yet-persisted deep link. Activation saves first, then navigates.
      href: fallbackHref,
      title: "This large Selection A will be saved locally before it opens.",
      onClick: (event: Event) => {
        event.preventDefault();
        const link = event.currentTarget as HTMLElement | null;
        save(link);
      },
    };
  }
  const href = serialized.hash;
  return {
    href,
    onClick: (event: Event) => {
      const pointer = event as MouseEvent;
      if (pointer.button !== 0 || pointer.metaKey || pointer.ctrlKey || pointer.shiftKey || pointer.altKey) return;
      event.preventDefault();
      context.analysis.setSelection(selection);
      context.analysis.navigateWorkspace(workspace);
      window.location.hash = href;
    },
  };
}

let savedSelectionSequence = 0;

function renderAttentionQueue(
  rows: AttnRow[],
  client: TareClient,
  context: WorkspaceContext | undefined,
  onSaveError: (error: unknown, retry: () => void) => void
): HTMLElement {
  const section = el("section", { class: "pulse-attention", "aria-labelledby": "pulse-attn-h" }, [
    el("h2", { id: "pulse-attn-h", class: "subhead", text: "Needs attention" }),
  ]);
  if (rows.length === 0) {
    section.appendChild(el("p", { class: "caption sub", text: "Nothing needs attention in this period." }));
    return section;
  }
  const list = el("ol", { class: "pulse-attn-list" });
  const scope = context?.analysis.get().scope;
  for (const r of rows) {
    // Budget and capture remedies are utility surfaces, not analytical cohorts. Preserve their
    // direct sheet destinations; only evidence-bearing rows carry Selection A across workspaces.
    const carriesSelection = r.kind !== "budget" && r.kind !== "capture";
    const link = scope && carriesSelection
      ? selectionLink(attentionSelection(r, scope), r.href, client, context, onSaveError)
      : { href: r.href };
    // A plain-language sentence row (NOT a card) that opens its evidence. The dollar impact is a
    // right-aligned figure; a capture gap shows "uncaptured" rather than a fabricated $0.
    list.appendChild(
      el("li", {}, [
        el("a", { class: `pulse-attn-row attn-${r.kind}`, ...link }, [
          el("span", { class: "pulse-attn-text", text: r.sentence }),
          el("span", {
            class: "pulse-attn-dollars num",
            text: r.dollars >= 0 ? fmtUsd(r.dollars) : "uncaptured",
          }),
          el("span", {
            class: "pulse-row-action",
            text:
              r.kind === "capture"
                ? "Fix capture ›"
                : r.kind === "budget"
                  ? "Review budget ›"
                  : r.kind === "failure" || r.kind === "loop"
                    ? "Review in Optimize ›"
                    : "Investigate ›",
          }),
        ]),
      ])
    );
  }
  section.appendChild(list);
  return section;
}

// ---- Controllable drivers -------------------------------------------

/// Ranked controllable drivers from the savings ledger: each is a quantified action with an explicit
/// recoverable estimate + CONFIDENCE (measured/projected/approximate) + effort — never an unlabeled
/// promise. Deep-links to the opportunity's evidence run when it has one.
/// Wrap the Pulse spend-anatomy Beam in a labelled section, or nothing when there is no priced
/// spend to partition (`pulseBeamModel` returns null). Kept thin so the model stays purely testable.
function beamSection(savings: SavingsLedger, confidence: EstimateConfidence): HTMLElement | null {
  const model = pulseBeamModel(savings, confidence.unpriced_token_share_pct);
  if (!model) return null;
  return el("div", { class: "pulse-beam" }, [tareBeam(model)]);
}

function renderDrivers(
  ledger: SavingsLedger,
  client: TareClient,
  context: WorkspaceContext | undefined,
  onSaveError: (error: unknown, retry: () => void) => void
): HTMLElement {
  const section = el("section", { class: "pulse-drivers", "aria-labelledby": "pulse-drv-h" }, [
    el("h2", { id: "pulse-drv-h", class: "subhead", text: "Top controllable drivers" }),
  ]);
  const opps = [...(ledger.opportunities ?? [])]
    .filter((o) => o.recoverable_micros > 0)
    .sort((a, b) => b.recoverable_micros - a.recoverable_micros)
    .slice(0, 5);
  const v2 = ledger.opportunities_v2 ?? [];
  if (opps.length === 0) {
    section.appendChild(el("p", { class: "caption sub", text: "No controllable drivers identified in this period." }));
    return section;
  }
  const list = el("ol", { class: "pulse-drv-list" });
  for (const o of opps) {
    // Prefer a v2 match's affected run as the evidence deep-link; else the Optimize worklist.
    const match = v2.find((x) => x.label === o.label && x.kind === o.kind);
    const runId = match?.affected_run_ids?.[0];
    const fallbackHref = runId
      ? routePath(["investigate", "run", runId])
      : routePath(["optimize"]);
    const scope = context?.analysis.get().scope;
    const link = scope
      ? selectionLink(driverSelection(match, scope), fallbackHref, client, context, onSaveError)
      : { href: fallbackHref };
    list.appendChild(
      el("li", {}, [
        el("a", { class: `pulse-drv-row drv-${o.kind}`, ...link }, [
          el("span", { class: "pulse-drv-label", text: o.label }),
          el("span", { class: "pulse-drv-fix sub", text: o.fix_text }),
          el("span", { class: "pulse-drv-meta" }, [
            el("span", { class: "pulse-drv-save num", text: `${fmtUsd(o.recoverable_micros)} recoverable` }),
            // Every action carries its confidence + effort — never an unlabeled savings promise.
            el("span", { class: `pulse-drv-conf conf-${o.confidence}`, text: `${o.confidence} · effort ${o.effort}` }),
            el("span", { class: "pulse-row-action", text: "Review fix ›" }),
          ]),
        ]),
      ])
    );
  }
  section.appendChild(list);
  // Honest ledger framing: capped potential, estimated, never a deduped floor.
  section.appendChild(
    el("p", {
      class: "caption sub",
      text: `Estimated capped potential — ${fmtUsd(ledger.total_recoverable_micros)} recoverable of ${fmtUsd(ledger.total_spend_micros)} spend (not a guaranteed floor).`,
    })
  );
  return section;
}

/// Render the Pulse workspace top hierarchy. `route` is accepted for parity with the workspace
/// signature (scope/sheet state lands in later Pulse tasks); all figures share one canonical load.
export async function renderPulse(
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context?: WorkspaceContext
): Promise<void> {
  let burn: BurnRate;
  let cov: Coverage;
  let anomalies: Anomaly[];
  let budget: PeriodBudget;
  let failures: FailureWasteReport;
  let loops: LoopWasteReport;
  let savings: SavingsLedger;
  let confidence: EstimateConfidence;
  const range = requestedRange(route);
  try {
    // One canonical, scope-sharing load: every figure below derives from this single batch so they
    // share scope (no per-card refetch, no drifting totals).
    [burn, cov, anomalies, budget, failures, loops, savings, confidence] = await Promise.all([
      client.burnrate(range),
      client.coverage(),
      client.anomalies({ by: "model" }),
      client.budget(),
      client.failures(),
      client.loops(),
      client.savings(),
      client.confidence(),
    ]);
  } catch (e) {
    root.replaceChildren(
      errorNode("Pulse couldn't load the selected date range.", e, {
        actions: [
          { label: "Retry", primary: true, run: () => renderPulse(root, client, route, context) },
          { label: "Open Investigate", href: "#/investigate" },
        ],
      })
    );
    return;
  }

  const attention = buildAttentionRows({ anomalies, budget, failures, loops, coverage: cov });
  const selectionStatus = el("div", {
    class: "pulse-selection-status",
    "aria-live": "assertive",
  });
  const reportSaveError = (error: unknown, retry: () => void): void => {
    selectionStatus.replaceChildren(
      errorNode("Couldn't save this large Selection A, so it was not opened.", error, {
        actions: [{ label: "Retry save", primary: true, run: retry }],
      })
    );
  };

  const section = el("section", { class: "pulse" }, [
    // 1. URL-backed chart range. Presets query real calendar windows; Custom routes to Investigate,
    // where arbitrary dates resolve through CohortSpec instead of inventing forecast semantics.
    el("div", { class: "pulse-scopebar" }, [
      el("div", { class: "pulse-scope-heading" }, [
        // Not an <h1>: the breadcrumb leaf is the page heading, so this visual title stays semantic
        // body copy and avoids a duplicate document heading.
        el("p", { class: "pulse-title", text: "Pulse" }),
        el("span", {
          class: "pulse-scope sub",
          text: `${periodScopeLabel(burn)}${burn.cap_micros > 0 ? " · vs cap pace" : ""}`,
        }),
      ]),
      pulseRangeSelector(burn, route),
    ]),
    // 2. One spend answer + comparable delta.
    spendAnswer(burn, cov, route),
    // 3. Actual daily cumulative history + pace scenarios and accessible data table.
    forecastChart(burn, cov, route),
    // 4. Capture provenance is part of the chart footer; blind sources become an actionable row below.
    // 5. Dollar-ranked attention queue (sentence rows, deep-linked to evidence).
    renderAttentionQueue(attention, client, context, reportSaveError),
    // 6. Top controllable drivers with quantified action + confidence.
    renderDrivers(savings, client, context, reportSaveError),
    // 6b. Spend-anatomy Beam — one money lane (recoverable + remainder = scoped priced spend); unpriced
    //     usage sits outside as a detached token-share marker. Reflects the same scoped batch, so it
    //     re-renders with scope changes; reduced-motion + 330→ultrawide responsiveness are the .tare-beam
    // CSS. Omitted when there is no priced spend to partition.
    beamSection(savings, confidence),
    selectionStatus,
    // 7. The Now feed — active sessions + recent expensive steps (bounded, self-cancelling poll).
    renderNowFeed(client),
  ]);
  root.replaceChildren(section);
}
