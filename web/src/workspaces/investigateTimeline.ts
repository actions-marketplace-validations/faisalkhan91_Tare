// Canonical Investigate timeline. This is a SCREEN-ONLY line chart:
// it deliberately does not reuse or modify the byte-stable export SVG renderer. Daily values come
// from the shared scoped timeline endpoint, not the legacy unscoped trend totals. The legacy trend
// is consulted only for a default calendar extent when the durable scope is unbounded.

import { baselineLabel, priorWindowBaseline, type BaselineSpec } from "../analysis/state.js";
import { canonicalCohortJson } from "../analysis/serialize.js";
import { serializeAnalysisHash } from "../analysis/store.js";
import type {
  CohortMetric,
  CohortSpec,
  Normalization,
  SavingsAction,
  TimelineGroup,
  TimelineSeriesResult,
  TimelineUnit,
} from "../analysis/types.js";
import type { TareClient } from "../client.js";
import type { WorkspaceContext } from "../shell/workbench.js";
import { el, rawSvg } from "../ui/el.js";
import { fmtUsd, humanizeKey } from "../ui/format.js";
import { addDays } from "../ui/range.js";
import { parseHash, routePath } from "../ui/store.js";

export const TIMELINE_GROUPS = ["total", "provider", "model", "cause"] as const;
export type { TimelineGroup } from "../analysis/types.js";

export interface TimelineSeries {
  key: string;
  observed: Array<number | null>;
  comparison: Array<number | null>;
}

export interface TimelineAnnotation {
  date: string;
  kind: "intervention" | "config_change";
  label: string;
}

export interface InvestigateTimelineModel {
  days: string[];
  baselineDays: string[];
  quarantinedDays: string[];
  localToday: string;
  observed: CohortSpec;
  baseline: BaselineSpec;
  selectionSampleCount: number;
  baselineSampleCount: number;
  series: TimelineSeries[];
  annotations: TimelineAnnotation[];
  pricingVersion: string;
  normalization: Normalization;
  unit: TimelineUnit;
  unavailableReasons: string[];
}

export class TimelineCapabilityError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TimelineCapabilityError";
  }
}

function localCalendarDate(instant: Date, timezone: string): string {
  try {
    const parts = new Intl.DateTimeFormat("en-US", {
      timeZone: timezone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
    }).formatToParts(instant);
    const part = (kind: Intl.DateTimeFormatPartTypes): string | undefined =>
      parts.find((candidate) => candidate.type === kind)?.value;
    const year = part("year");
    const month = part("month");
    const day = part("day");
    if (year && month && day) return `${year}-${month}-${day}`;
  } catch {
    // A CohortSpec normally carries a validated IANA zone. If a stale/legacy value reaches this
    // presentation boundary, fail closed to UTC rather than declaring an unknown local day complete.
  }
  return instant.toISOString().slice(0, 10);
}

function actionCalendarDate(action: SavingsAction, timezone: string): string | null {
  const instant = new Date(action.acted_at);
  return Number.isFinite(instant.getTime()) ? localCalendarDate(instant, timezone) : null;
}

/// A day is complete only after its requested timezone's calendar day has ended. Current and future
/// days are quarantined from the brush and all comparison arithmetic; no capture-completeness claim
/// is inferred for historical days because the counts ledger has no denominator for that.
export function quarantineIncompleteDays(
  days: string[],
  timezone: string,
  now = new Date()
): { complete: string[]; quarantined: string[]; localToday: string } {
  const localToday = localCalendarDate(now, timezone);
  return {
    complete: days.filter((day) => day < localToday),
    quarantined: days.filter((day) => day >= localToday),
    localToday,
  };
}

function calendarDays(from: string, to: string): string[] {
  if (from > to) return [];
  const days: string[] = [];
  let day = from;
  // A defensive ceiling prevents a malformed hand-authored link from locking a WebView. Normal
  // scopes are 7/30/90 days; a larger valid scope gets a visible error instead of silent truncation.
  while (day <= to && days.length <= 366) {
    days.push(day);
    day = addDays(day, 1);
  }
  if (days.length > 366) throw new TimelineCapabilityError("Timeline ranges over one year must be narrowed before rendering.");
  return days;
}

function sameTimelineCohort(a: CohortSpec, b: CohortSpec): boolean {
  return canonicalCohortJson({ ...a, from: null, to: null }) === canonicalCohortJson({ ...b, from: null, to: null });
}

function buildSeries(
  observed: TimelineSeriesResult[],
  comparison: TimelineSeriesResult[],
  observedDays: string[],
  comparisonDays: string[],
  normalization: Normalization,
  metric: CohortMetric
): TimelineSeries[] {
  const observedBy = new Map(observed.map((series) => [series.key, series]));
  const comparisonBy = new Map(comparison.map((series) => [series.key, series]));
  const keys = new Set([...observedBy.keys(), ...comparisonBy.keys()]);
  const values = (
    series: TimelineSeriesResult | undefined,
    peers: TimelineSeriesResult[],
    days: string[]
  ): Array<number | null> => {
    if (!series) {
      return days.map((day) => {
        if (normalization === "absolute" && metric !== "cache_hit_rate") return 0;
        if (normalization === "per_run" || metric === "cache_hit_rate") return null;
        const hasDenominator = peers
          .flatMap((candidate) => candidate.points)
          .some((point) => point.day === day && point.denominator != null && point.denominator > 0);
        return hasDenominator ? 0 : null;
      });
    }
    const byDay = new Map(series.points.map((point) => [point.day, point.value]));
    return days.map((day) => byDay.get(day) ?? null);
  };
  const total = (row: Array<number | null>): number =>
    row.reduce<number>((sum, value) => sum + (value ?? 0), 0);
  return [...keys]
    .map((key) => ({
      key,
      observed: values(observedBy.get(key), observed, observedDays),
      comparison: values(comparisonBy.get(key), comparison, comparisonDays),
    }))
    .sort((a, b) => total(b.observed) - total(a.observed) || a.key.localeCompare(b.key));
}

async function defaultExtent(client: TareClient, scope: CohortSpec): Promise<string[]> {
  if (scope.from && scope.to) return calendarDays(scope.from, scope.to);
  const extent = await client.trend({
    from: scope.from ?? undefined,
    to: scope.to ?? undefined,
    by: "total",
  });
  return [...extent.days];
}

/// Load one scoped timeline. Every plotted dollar is resolved against the exact CohortSpec (filters,
/// timezone, pricing mode, entity grain); the old trend values are never substituted. Nullable
/// endpoint points retain their explicit unavailable reasons instead of becoming zeroes.
export async function loadInvestigateTimeline(
  client: TareClient,
  scope: CohortSpec,
  baselineSpec: BaselineSpec | null,
  group: TimelineGroup,
  now = new Date()
): Promise<InvestigateTimelineModel> {
  const extent = await defaultExtent(client, scope);
  const split = quarantineIncompleteDays(extent, scope.timezone, now);
  if (split.complete.length === 0) {
    throw new TimelineCapabilityError(
      split.quarantined.length > 0
        ? `Only the still-open ${scope.timezone} calendar day is available; it is quarantined until complete.`
        : "No captured calendar days are available for this scope."
    );
  }
  const observed: CohortSpec = {
    ...scope,
    from: split.complete[0],
    to: split.complete[split.complete.length - 1],
  };
  if (baselineSpec && (!baselineSpec.cohort.from || !baselineSpec.cohort.to)) {
    throw new TimelineCapabilityError(
      `${baselineLabel(baselineSpec.kind)} Baseline B has no bounded calendar window. Update or clear it before timeline comparison; it is not silently rewritten.`
    );
  }
  const baseline = baselineSpec ?? priorWindowBaseline(observed);
  if (!baseline?.cohort.from || !baseline.cohort.to) {
    throw new TimelineCapabilityError("A bounded window is required to derive the equal prior-window baseline.");
  }
  const [observedResponse, baselineResponse, actions] = await Promise.all([
    client.timelineCohort({ cohort: observed, group }),
    client.timelineCohort({ cohort: baseline.cohort, group }),
    client.savingsActions().catch(() => [] as SavingsAction[]),
  ]);
  const measuredBaseline: BaselineSpec = {
    ...baseline,
    label: baselineLabel(baseline.kind, baselineResponse.data.run_count),
    sampleCount: baselineResponse.data.run_count,
  };
  const interventionAnnotations: TimelineAnnotation[] = actions
    .filter((action) => action.status === "applied" && sameTimelineCohort(action.cohort, observed))
    .map((action) => ({ action, date: actionCalendarDate(action, scope.timezone) }))
    .filter((entry): entry is { action: SavingsAction; date: string } => entry.date != null)
    .filter((entry) => entry.date >= split.complete[0] && entry.date <= split.complete[split.complete.length - 1])
    .map(({ action, date }) => ({
      date,
      kind: "intervention" as const,
      label: `Applied · ${action.opportunity_key}`,
    }));
  const configAnnotations: TimelineAnnotation[] = observedResponse.data.config_events.map((event) => ({
    date: event.day,
    kind: "config_change",
    label: `Configuration changed · ${event.changed_fields.join(", ")}`,
  }));
  const unavailableReasons = [...observedResponse.data.series, ...baselineResponse.data.series]
    .flatMap((series) => series.points.map((point) => point.unavailable_reason))
    .filter((reason): reason is string => Boolean(reason))
    .filter((reason, index, all) => all.indexOf(reason) === index);
  const observedKeys = new Set(observedResponse.data.series.map((series) => series.key));
  const baselineKeys = new Set(baselineResponse.data.series.map((series) => series.key));
  const asymmetricSeries = [...observedKeys, ...baselineKeys]
    .some((key) => !observedKeys.has(key) || !baselineKeys.has(key));
  if (asymmetricSeries && scope.normalization === "per_run") {
    unavailableReasons.push("Per-run values are unavailable where no selected entities carry the series.");
  }
  if (asymmetricSeries && scope.metric === "cache_hit_rate") {
    unavailableReasons.push("Cache hit rate is unavailable where the series has no cache input denominator.");
  }

  return {
    days: observedResponse.data.days,
    baselineDays: baselineResponse.data.days,
    quarantinedDays: split.quarantined,
    localToday: split.localToday,
    observed,
    baseline: measuredBaseline,
    selectionSampleCount: observedResponse.data.run_count,
    baselineSampleCount: baselineResponse.data.run_count,
    series: buildSeries(
      observedResponse.data.series,
      baselineResponse.data.series,
      observedResponse.data.days,
      baselineResponse.data.days,
      scope.normalization,
      scope.metric
    ),
    annotations: [...interventionAnnotations, ...configAnnotations].sort((a, b) =>
      a.date.localeCompare(b.date) || a.kind.localeCompare(b.kind) || a.label.localeCompare(b.label)
    ),
    pricingVersion: observedResponse.provenance.pricing_edition.version,
    normalization: scope.normalization,
    unit: observedResponse.data.unit,
    unavailableReasons,
  };
}

function esc(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function metricValue(value: number, unit: TimelineUnit): string {
  if (unit === "percent") return `${value.toFixed(1)}%`;
  if (unit === "tokens" || unit === "tokens_per_run" || unit === "tokens_per_outcome") {
    return `${Math.round(value).toLocaleString()} tokens`;
  }
  return fmtUsd(value);
}

function timelineSvg(model: InvestigateTimelineModel): string {
  const width = 760;
  const height = 250;
  const left = 42;
  const right = 16;
  const top = 24;
  const bottom = 38;
  const plotW = width - left - right;
  const plotH = height - top - bottom;
  const all = model.series
    .flatMap((series) => [...series.observed, ...series.comparison])
    .filter((value): value is number => value != null && Number.isFinite(value));
  const max = Math.max(0, ...all);
  const denom = max > 0 ? max : 1;
  const n = Math.max(model.days.length, model.baselineDays.length);
  const x = (index: number): number => left + (n <= 1 ? plotW / 2 : (index / (n - 1)) * plotW);
  const y = (value: number): number => top + plotH - (Math.max(0, value) / denom) * plotH;
  const path = (values: Array<number | null>): string => {
    let open = false;
    return values.flatMap((value, index) => {
      if (value == null || !Number.isFinite(value)) {
        open = false;
        return [];
      }
      const command = open ? "L" : "M";
      open = true;
      return [`${command}${x(index).toFixed(2)} ${y(value).toFixed(2)}`];
    }).join(" ");
  };
  const out: string[] = [
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}" role="img" aria-label="Observed scoped timeline with synchronized baseline comparison">`,
    `<line x1="${left}" y1="${top + plotH}" x2="${width - right}" y2="${top + plotH}" stroke="var(--border-strong)"/>`,
    `<text x="${left}" y="12" fill="var(--muted)" font-size="10">${esc(metricValue(max, model.unit))} peak · ${esc(humanizeKey(model.normalization))}</text>`,
  ];
  model.annotations.forEach((annotation) => {
    const index = model.days.indexOf(annotation.date);
    if (index < 0) return;
    out.push(`<line x1="${x(index)}" y1="${top}" x2="${x(index)}" y2="${top + plotH}" stroke="var(--cat-6)" stroke-dasharray="2 3"><title>${esc(annotation.date)} · ${esc(annotation.label)}</title></line>`);
  });
  model.series.forEach((series, index) => {
    const color = `var(--cat-${(index % 6) + 1})`;
    if (series.comparison.length > 0) {
      out.push(`<path d="${path(series.comparison)}" fill="none" stroke="${color}" stroke-width="1.5" stroke-dasharray="6 4" opacity="0.72"/>`);
    }
    if (series.observed.length > 0) {
      out.push(`<path d="${path(series.observed)}" fill="none" stroke="${color}" stroke-width="2.5"/>`);
      series.observed.forEach((value, dayIndex) => {
        if (value == null || !Number.isFinite(value)) return;
        out.push(`<circle cx="${x(dayIndex)}" cy="${y(value)}" r="3" fill="${color}"><title>${esc(model.days[dayIndex])} · ${esc(series.key)} · ${esc(metricValue(value, model.unit))}</title></circle>`);
      });
    }
  });
  out.push(
    `<text x="${left}" y="${height - 10}" fill="var(--muted)" font-size="10">${esc(model.days[0])}</text>`,
    `<text x="${width - right}" y="${height - 10}" fill="var(--muted)" font-size="10" text-anchor="end">${esc(model.days[model.days.length - 1])}</text>`,
    "</svg>"
  );
  return out.join("");
}

function timelineHash(hash: string, group: TimelineGroup): string {
  const parsed = parseHash(hash);
  return routePath(parsed.segments, { ...(parsed.query ?? {}), group, mode: "timeline" });
}

function selectControl(
  className: string,
  label: string,
  value: string,
  choices: Array<{ value: string; label: string }>
): { wrap: HTMLElement; select: HTMLSelectElement } {
  const select = el("select", { class: className, "aria-label": label },
    choices.map((choice) => el("option", {
      value: choice.value,
      text: choice.label,
      selected: choice.value === value,
    }))
  ) as HTMLSelectElement;
  return {
    select,
    wrap: el("label", { class: "inv-timeline-control" }, [el("span", { class: "caption", text: label }), select]),
  };
}

function stateWithMetric(context: WorkspaceContext, metric: CohortMetric, normalization: Normalization) {
  const current = context.analysis.get();
  const update = (cohort: CohortSpec): CohortSpec => ({ ...cohort, metric, normalization });
  return {
    ...current,
    workspace: "investigate" as const,
    scope: update(current.scope),
    selection: current.selection ? update(current.selection) : null,
    baseline: current.baseline ? { ...current.baseline, cohort: update(current.baseline.cohort) } : null,
  };
}

function navigateTimelineState(context: WorkspaceContext, next: ReturnType<typeof stateWithMetric>, group: TimelineGroup): void {
  const serialized = serializeAnalysisHash(["investigate"], next);
  if (serialized.kind !== "inline") return;
  context.analysis.set(next);
  window.location.hash = timelineHash(serialized.hash, group);
}

/// Render controls, screen-only line chart, accessible two-handle brush, measured baseline facts,
/// applied-intervention annotations, and explicit capability/completeness disclosures.
export async function renderInvestigateTimeline(
  host: HTMLElement,
  client: TareClient,
  scope: CohortSpec,
  selection: CohortSpec | null,
  baseline: BaselineSpec | null,
  context: WorkspaceContext | undefined,
  query: Record<string, string>
): Promise<void> {
  const requestedGroup = query.group as TimelineGroup | undefined;
  const group: TimelineGroup = requestedGroup && TIMELINE_GROUPS.includes(requestedGroup) ? requestedGroup : "total";
  const metric = selectControl("inv-timeline-metric", "Metric", scope.metric, [
    { value: "spend_micros", label: "Estimated spend" },
    { value: "tokens", label: "Tokens" },
    { value: "cache_hit_rate", label: "Cache hit rate" },
  ]);
  const normalization = selectControl("inv-timeline-normalization", "Normalization", scope.normalization, [
    { value: "absolute", label: "Absolute" },
    { value: "per_run", label: "Per run" },
    { value: "share_of_selection", label: "Share of selection" },
    { value: "per_outcome", label: "Per outcome" },
  ]);
  const grouping = selectControl("inv-timeline-group", "Group", group, [
    { value: "total", label: "Total" },
    { value: "provider", label: "Provider" },
    { value: "model", label: "Model" },
    { value: "cause", label: "Cause" },
  ]);
  const shell = el("section", { class: "inv-timeline", "aria-label": "Timeline analysis" }, [
    el("div", { class: "inv-timeline-tools" }, [metric.wrap, normalization.wrap, grouping.wrap]),
  ]);
  host.replaceChildren(shell);

  if (context) {
    metric.select.addEventListener("change", () => {
      const nextMetric = metric.select.value as CohortMetric;
      const nextNormalization: Normalization = nextMetric === "cache_hit_rate"
        ? "absolute"
        : normalization.select.value as Normalization;
      navigateTimelineState(context, stateWithMetric(context, nextMetric, nextNormalization), group);
    });
    normalization.select.addEventListener("change", () => {
      const nextNormalization = normalization.select.value as Normalization;
      navigateTimelineState(context, stateWithMetric(context, metric.select.value as CohortMetric, nextNormalization), group);
    });
    grouping.select.addEventListener("change", () => {
      const nextGroup = grouping.select.value as TimelineGroup;
      const serialized = serializeAnalysisHash(["investigate"], context.analysis.get());
      if (serialized.kind === "inline") window.location.hash = timelineHash(serialized.hash, nextGroup);
    });
  }

  // The scope supplies the calendar extent. Existing Selection A filters remain active so returning
  // from a Pulse anomaly does not silently widen the plotted cohort back to all runs.
  const active: CohortSpec = selection
    ? { ...selection, from: scope.from, to: scope.to }
    : scope;
  let model: InvestigateTimelineModel;
  try {
    model = await loadInvestigateTimeline(client, active, baseline, group);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    shell.appendChild(el("div", { class: "notice warn inv-timeline-unavailable", role: "status" }, [
      el("strong", { text: "Timeline unavailable for this mode. " }),
      document.createTextNode(message),
    ]));
    shell.appendChild(el("p", {
      class: "caption sub",
      text: "Configuration-change markers come only from persisted local Settings events; observed config mix is never mislabeled as a change event.",
    }));
    return;
  }

  const chart = rawSvg(el("div", {
    class: "inv-timeline-chart",
    tabindex: "0",
    role: "group",
    "aria-label": "Timeline chart; scroll horizontally to view all dates",
  }), timelineSvg(model));
  shell.appendChild(chart);
  shell.appendChild(el("div", { class: "inv-timeline-legend", "aria-label": "Line styles" }, [
    el("span", { class: "caption inv-line-observed", text: "Observed scoped cohort" }),
    el("span", { class: "caption inv-line-baseline", text: "Baseline B · synchronized by window position" }),
    el("span", { class: "caption inv-line-intervention", text: "Persisted local event" }),
  ]));
  const valueBasis = scope.metric === "spend_micros"
    ? `Estimated spend · pricing ${model.pricingVersion}`
    : scope.metric === "tokens"
      ? "Observed provider token counts"
      : "Observed cache-read share of captured input tokens";
  shell.appendChild(el("dl", { class: "inv-timeline-facts" }, [
    el("div", {}, [el("dt", { text: "Observed window" }), el("dd", { text: `${model.days[0]}–${model.days[model.days.length - 1]} · ${model.selectionSampleCount} runs` })]),
    el("div", {}, [el("dt", { text: "Baseline B" }), el("dd", { text: `${model.baseline.label} · ${model.baselineDays[0]}–${model.baselineDays[model.baselineDays.length - 1]}` })]),
    el("div", {}, [el("dt", { text: "Value basis" }), el("dd", { text: `${valueBasis} · ${scope.timezone}` })]),
  ]));

  if (model.unavailableReasons.length > 0) {
    shell.appendChild(el("div", { class: "notice warn inv-timeline-unavailable-points", role: "status" }, [
      el("strong", { text: "Some timeline values are unavailable. " }),
      document.createTextNode(model.unavailableReasons.join(" ")),
    ]));
  }

  if (model.annotations.length > 0) {
    shell.appendChild(el("ul", { class: "inv-timeline-annotations", "aria-label": "Timeline annotations" },
      model.annotations.map((annotation) => el("li", { text: `${annotation.date} · ${annotation.label}` }))
    ));
  }
  if (!model.annotations.some((annotation) => annotation.kind === "config_change")) {
    shell.appendChild(el("p", {
      class: "caption sub inv-timeline-config-gap",
      text: "Configuration changes · no persisted Settings changes in this window; captured run mix is never used as a substitute.",
    }));
  }
  if (model.quarantinedDays.length > 0) {
    shell.appendChild(el("div", { class: "notice warn inv-timeline-quarantine", role: "status" }, [
      el("strong", { text: "Incomplete days quarantined. " }),
      document.createTextNode(`${model.quarantinedDays.join(", ")} are current/future days in ${scope.timezone} and cannot be brushed or compared yet.`),
    ]));
  }

  const start = el("input", {
    type: "range", min: 0, max: model.days.length - 1, value: 0,
    class: "inv-brush-start", "aria-label": "Selection A start day",
  }) as HTMLInputElement;
  const end = el("input", {
    type: "range", min: 0, max: model.days.length - 1, value: model.days.length - 1,
    class: "inv-brush-end", "aria-label": "Selection A end day",
  }) as HTMLInputElement;
  if (selection?.from && model.days.includes(selection.from)) start.value = String(model.days.indexOf(selection.from));
  if (selection?.to && model.days.includes(selection.to)) end.value = String(model.days.indexOf(selection.to));
  const output = el("output", { class: "inv-brush-output caption", "aria-live": "polite" });
  const apply = el("button", { class: "btn primary inv-brush-apply", type: "button", text: "Apply brushed range" }) as HTMLButtonElement;
  const brush = el("fieldset", { class: "inv-brush" }, [
    el("legend", { text: "Brush timeline into Selection A" }),
    el("label", { class: "caption", text: "Start" }), start,
    el("label", { class: "caption", text: "End" }), end,
    output,
    apply,
  ]);
  const updateOutput = (): { from: string; to: string } => {
    let a = Number(start.value);
    let b = Number(end.value);
    if (a > b) [a, b] = [b, a];
    const value = { from: model.days[a], to: model.days[b] };
    output.textContent = `Selection A · ${value.from}–${value.to} · Baseline B will use the immediately preceding ${b - a + 1} day(s).`;
    return value;
  };
  start.addEventListener("input", updateOutput);
  end.addEventListener("input", updateOutput);
  updateOutput();
  apply.addEventListener("click", () => {
    if (!context) return;
    const range = updateOutput();
    apply.disabled = true;
    apply.setAttribute("aria-busy", "true");
    const selected: CohortSpec = { ...active, from: range.from, to: range.to };
    const prior = priorWindowBaseline(selected);
    if (!prior) return;
    void Promise.all([client.resolveCohort(selected), client.resolveCohort(prior.cohort)])
      .then(([selectionResult, baselineResult]) => {
        const measured: BaselineSpec = {
          ...prior,
          label: baselineLabel("prior_window", baselineResult.data.run_count),
          sampleCount: baselineResult.data.run_count,
        };
        const next = {
          ...context.analysis.get(),
          workspace: "investigate" as const,
          selection: selected,
          baseline: measured,
        };
        const serialized = serializeAnalysisHash(["investigate"], next);
        if (serialized.kind !== "inline") {
          output.textContent = `${serialized.reason}. Save the investigation before navigating.`;
          return;
        }
        context.analysis.set({ selection: selected, baseline: measured });
        output.textContent = `Selection A · ${range.from}–${range.to} · ${selectionResult.data.run_count} runs. Baseline B · ${measured.label} · ${measured.cohort.from}–${measured.cohort.to}.`;
        window.location.hash = timelineHash(serialized.hash, group);
      })
      .catch((error) => {
        output.textContent = `Could not resolve the brushed cohorts: ${error instanceof Error ? error.message : String(error)}`;
      })
      .finally(() => {
        apply.disabled = false;
        apply.removeAttribute("aria-busy");
      });
  });
  shell.appendChild(brush);
}
