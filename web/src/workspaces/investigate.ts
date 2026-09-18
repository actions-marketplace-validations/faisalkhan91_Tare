// Investigate workspace. Explore a cohort: an entity/grouping MODE
// selector (Runs / Sessions / Templates / Steps / Time), privacy-safe SERVER search, cohort FILTER
// tokens, and a ranked-bars + sortable/keyboard result table whose rows update the canonical selection
// — all without ever resetting the scope. The inspector includes distinguishing dimensions and
// representative runs; table rows deep-link while preserving scope.
// The nested canonical routes investigate/run/:id + investigate/compare keep rendering their evidence.
// Run Profile is lazy even within Investigate so the facet workspace does not pay for it until opened.
//
// Invariants: search stays privacy-safe (searchCohort over IDs/labels/hashes/tags/notes/model/config —
// NEVER payload text); filter tokens map 1:1 to CohortFilter semantics; the result table reuses
// the shared dataTable primitive for sorting + the keyboard core loop; every navigation preserves the
// scope query (from/to/tz/…). Framework-free + jsdom-testable: the cohort builder + loaders are pure.

import { el } from "../ui/el.js";
import { metricLabel, normalizationLabel } from "../analysis/labels.js";
import { dataTable, type Column } from "../ui/datatable.js";
import { parseHash, routeHash, routePath, setRouteQuery } from "../ui/store.js";
import { fmtUsd, fmtTokens } from "../ui/format.js";
import { errorNode } from "../ui/errorNode.js";
import { icon } from "../ui/icon.js";
import { renderCompare } from "../screens/compare.js";
import { renderInspector } from "./investigateInspector.js";
import { installAdaptivePaneInteractions } from "../shell/adaptivePanes.js";
import { serializeAnalysisHash } from "../analysis/store.js";
import { cohortHash, encodeCohort, encodeFilters } from "../analysis/serialize.js";
import { investigationFromState } from "../analysis/investigation.js";
import type { Route } from "../ui/store.js";
import type { WorkspaceContext } from "../shell/workbench.js";
import type { TareClient } from "../client.js";
import type { AnalysisState, BaselineSpec } from "../analysis/state.js";
import type { CohortSpec, CohortFilter, SearchField, CohortDimension, FacetRow } from "../analysis/types.js";

export const MODES = ["runs", "sessions", "templates", "steps", "time"] as const;
export type Mode = (typeof MODES)[number];
const MODE_LABEL: Record<Mode, string> = {
  runs: "Runs",
  sessions: "Sessions",
  templates: "Templates",
  steps: "Steps",
  time: "Time",
};

// Search covers only IDENTIFYING fields — never captured payload text ( privacy invariant).
const SEARCH_FIELDS: SearchField[] = ["id", "label", "hash", "tag", "note", "model", "config"];

/// An honest default cohort (mirrors analysis/investigation.emptyScope): all fields explicit so it
/// resolves deterministically. The scope query (from/to/tz) overrides the dates when present; a small
/// safe set of query params becomes cohort filters (dimension/tag/threshold — never payload).
export function baseCohort(query: Record<string, string>): CohortSpec {
  const filters: CohortFilter[] = [];
  if (query.model) filters.push({ op: "eq", dimension: "model", value: query.model });
  if (query.provider) filters.push({ op: "eq", dimension: "provider", value: query.provider });
  if (query.session) filters.push({ op: "eq", dimension: "session", value: query.session });
  if (query.template) filters.push({ op: "eq", dimension: "template", value: query.template });
  if (query.tag) filters.push({ op: "tag", value: query.tag });
  if (query.minspend && Number.isFinite(Number(query.minspend))) {
    filters.push({ op: "gte_micros", value: Math.round(Number(query.minspend) * 1_000_000) });
  }
  return {
    from: query.from ?? null,
    to: query.to ?? null,
    timezone: query.tz ?? "UTC",
    entity: "run",
    filters,
    pricing: { mode: "effective_dated" },
    metric: "spend_micros",
    normalization: "absolute",
    outcome_denominator: null,
  };
}

/// Replace one faceted dimension without stacking contradictory equality filters. `null` clears the
/// dimension. Other selection filters (including the current date brush) remain intact, so a facet
/// is a real Selection-A refinement rather than a cosmetic query/highlight.
export function selectionWithFacet(
  cohort: CohortSpec,
  dimension: Extract<CohortDimension, "model" | "provider">,
  value: string | null
): CohortSpec {
  const filters = cohort.filters.filter(
    (filter) => !(
      (filter.op === "eq" || filter.op === "in") && filter.dimension === dimension
    )
  );
  if (value) filters.push({ op: "eq", dimension, value });
  return { ...cohort, filters };
}

function selectedFacetValue(
  cohort: CohortSpec | null,
  dimension: Extract<CohortDimension, "model" | "provider">
): string | null {
  if (!cohort) return null;
  const filter = cohort.filters.find((candidate) => candidate.op === "eq" && candidate.dimension === dimension);
  return filter?.op === "eq" ? filter.value : null;
}

function sameCohort(a: CohortSpec, b: CohortSpec): boolean {
  return encodeCohort(a) === encodeCohort(b);
}

function sameFilter(a: CohortFilter, b: CohortFilter): boolean {
  return encodeFilters([a]) === encodeFilters([b]);
}

function withoutFirstFilter(filters: CohortFilter[], target: CohortFilter): CohortFilter[] {
  const index = filters.findIndex((candidate) => sameFilter(candidate, target));
  return index < 0 ? [...filters] : filters.filter((_, candidateIndex) => candidateIndex !== index);
}

const LEGACY_FILTER_KEYS = ["model", "provider", "session", "template", "tag", "minspend"] as const;

function legacyQueryFilters(query: Record<string, string>): CohortFilter[] {
  if (query.sel || !LEGACY_FILTER_KEYS.some((key) => Boolean(query[key]))) return [];
  return baseCohort(query).filters;
}

function filterOwner(filter: CohortFilter): string {
  if (filter.op === "eq" || filter.op === "in") return `dimension:${filter.dimension}`;
  if (filter.op === "tag") return "tag";
  if (filter.op === "gte_micros") return "gte_micros";
  if (filter.op === "lte_micros") return "lte_micros";
  if (filter.op === "quality_range") return "quality_range";
  if (filter.op === "run_ids") return "run_ids";
  return "step_refs";
}

function applyLegacyFilters(cohort: CohortSpec, filters: CohortFilter[]): CohortSpec {
  let next = [...cohort.filters];
  for (const filter of filters) {
    const owner = filterOwner(filter);
    next = next.filter((candidate) => filterOwner(candidate) !== owner);
    next.push(filter);
  }
  return { ...cohort, filters: next };
}

type LinkNavigation = {
  href: string;
  onClick?: (event: Event) => void;
  title?: string;
};

const ANALYSIS_QUERY_KEYS = new Set([
  "f", "sel", "base", "base_kind", "base_n", "investigation",
  ...LEGACY_FILTER_KEYS,
]);
let savedFacetSelectionSequence = 0;

function viewOnlyQuery(query: Record<string, string>): Record<string, string> {
  return Object.fromEntries(Object.entries(query).filter(([key]) => !ANALYSIS_QUERY_KEYS.has(key)));
}

function explicitStateHash(hash: string, state: AnalysisState): string {
  const destination = parseHash(hash);
  return routePath(destination.segments, {
    ...(destination.query ?? {}),
    // An encoded empty filter array is intentional: unlike an omitted `f`, it clears a prior scope
    // filter during browser back/forward. `sel=none` is the matching explicit Selection-A marker.
    f: encodeFilters(state.scope.filters),
    sel: state.selection ? encodeCohort(state.selection) : "none",
  });
}

function plainActivation(event: Event): boolean {
  const pointer = event as MouseEvent;
  return pointer.button === 0 && !pointer.metaKey && !pointer.ctrlKey && !pointer.shiftKey && !pointer.altKey;
}

/// Build a complete, reloadable analysis-state destination for facet/filter actions. Large cohorts
/// use the same save-before-navigation contract as Pulse instead of truncating Selection A.
function analysisStateLink(
  nextState: AnalysisState,
  query: Record<string, string>,
  fallbackHref: string,
  label: string,
  client: TareClient,
  context: WorkspaceContext | undefined,
  onSaveError: (error: unknown) => void
): LinkNavigation {
  if (!context) return { href: fallbackHref };
  const routeQuery = viewOnlyQuery(query);
  const serialized = serializeAnalysisHash(["investigate"], nextState, undefined, routeQuery);
  if (serialized.kind === "inline") {
    const href = explicitStateHash(serialized.hash, nextState);
    return {
      href,
      onClick: (event: Event) => {
        if (!plainActivation(event)) return;
        event.preventDefault();
        context.analysis.set(nextState);
        context.analysis.navigateWorkspace("investigate");
        window.location.hash = href;
      },
    };
  }

  const snapshot = nextState.selection ?? nextState.scope;
  const id = `investigate-facet-${cohortHash(snapshot)}-${Date.now().toString(36)}-${(++savedFacetSelectionSequence).toString(36)}`;
  const saved = serializeAnalysisHash(["investigate"], nextState, id, routeQuery);
  if (saved.kind !== "inline") return { href: fallbackHref };
  let saving = false;
  return {
    href: fallbackHref,
    title: "This large Selection A will be saved locally before it opens.",
    onClick: (event: Event) => {
      if (!plainActivation(event)) return;
      event.preventDefault();
      if (saving) return;
      saving = true;
      const link = event.currentTarget as HTMLElement | null;
      link?.setAttribute("aria-busy", "true");
      const now = new Date().toISOString();
      void client.saveInvestigation(investigationFromState(id, label, nextState, now))
        .then(() => {
          context.analysis.set(nextState);
          context.analysis.navigateWorkspace("investigate");
          window.location.hash = saved.hash;
        })
        .catch(onSaveError)
        .finally(() => {
          saving = false;
          link?.removeAttribute("aria-busy");
        });
    },
  };
}

/// Human label + owning query key for a cohort filter token (describes the CohortFilter semantics).
export function filterToken(f: CohortFilter): { label: string; queryKey?: string } {
  switch (f.op) {
    case "eq":
      return { label: `${f.dimension} = ${f.value}`, queryKey: f.dimension };
    case "in":
      return { label: `${f.dimension} ∈ {${f.values.join(", ")}}` };
    case "gte_micros":
      return { label: `spend ≥ ${fmtUsd(f.value)}`, queryKey: "minspend" };
    case "lte_micros":
      return { label: `spend ≤ ${fmtUsd(f.value)}` };
    case "tag":
      return { label: `tag: ${f.value}`, queryKey: "tag" };
    case "quality_range":
      return { label: `quality ${f.min ?? "*"}–${f.max ?? "*"}` };
    case "run_ids":
      return { label: `${f.ids.length} run(s)` };
    case "step_refs":
      return { label: `${f.refs.length} step(s)` };
  }
}

/// A ranked result row, normalized across the entity/grouping modes.
export interface ResultRow {
  key: string;
  label: string;
  dollars: number;
  support: number | null; // group support count (facet modes); null for run/step/time
  href: string; // scope-preserving deep-link to the entity's evidence
}

/// Load the ranked results for `mode` from the canonical cohort APIs. runs/steps → resolveCohort
/// (entity grain); sessions/templates → facetCohort (grouped); time → trend daily totals. Ranked by
/// dollars descending. Exported for tests.
export async function loadResults(
  client: TareClient,
  mode: Mode,
  cohort: CohortSpec,
  query: Record<string, string>
): Promise<ResultRow[]> {
  const withScope = (extra: Record<string, string>): string =>
    routeHash("investigate", undefined, { ...query, ...extra });

  if (mode === "runs" || mode === "steps") {
    const res = await client.resolveCohort({ ...cohort, entity: mode === "steps" ? "step" : "run" });
    return res.data.entity_rows
      .map((r) => {
        const step = r.entity.step_ordinal;
        const key = step != null ? `${r.entity.run_id}#${step}` : r.entity.run_id;
        return {
          key,
          label: step != null ? `${r.entity.run_id} · step ${step}` : r.entity.run_id,
          dollars: r.matched_micros,
          support: null as number | null,
          // Canonical multi-segment run route, scope query carried — routePath keeps the
          // segments (routeHash would percent-encode the "/").
          href: routePath(["investigate", "run", r.entity.run_id], query),
        };
      })
      .sort((a, b) => b.dollars - a.dollars);
  }
  if (mode === "sessions" || mode === "templates") {
    const dimension = mode === "sessions" ? "session" : "template";
    const res = await client.facetCohort({ selection: cohort, baseline: cohort, dimension });
    return res.data.rows
      .map((r) => ({
        key: `${dimension}:${r.value}`,
        label: r.value || "(none)",
        dollars: r.selection_micros,
        support: r.selection_support as number | null,
        href: withScope({ entity: "runs", [dimension]: r.value }),
      }))
      .sort((a, b) => b.dollars - a.dollars);
  }
  // time — rank days by total spend (sum of the trend series' per-day values).
  const tr = await client.trend({ from: cohort.from ?? undefined, to: cohort.to ?? undefined, by: "total" });
  return tr.days
    .map((day, i) => ({
      key: `day:${day}`,
      label: day,
      dollars: tr.series.reduce((s, ser) => s + (ser.per_day[i] ?? 0), 0),
      support: null as number | null,
      href: withScope({ entity: "runs", from: day, to: day }),
    }))
    .sort((a, b) => b.dollars - a.dollars);
}

/// The mode selector — buttons that switch the entity/grouping mode, each preserving the scope query
/// (only `entity` changes) so switching modes NEVER resets the scope.
function modeSelector(active: Mode, query: Record<string, string>): HTMLElement {
  const bar = el("div", {
    class: "inv-modes",
    role: "tablist",
    "aria-label": "Entity mode",
    "aria-orientation": "horizontal",
  });
  const links: HTMLAnchorElement[] = [];
  for (const m of MODES) {
    const timeline = m === "time";
    const link = el("a", {
      id: `investigate-mode-${m}`,
      class: `inv-mode${m === active ? " active" : ""}`,
      role: "tab",
      href: routeHash("investigate", undefined, {
        ...query,
        entity: m,
        mode: timeline ? "timeline" : "",
        group: timeline ? query.group ?? "total" : "",
      }),
      "aria-selected": m === active ? "true" : "false",
      "aria-controls": "investigate-canvas",
      tabindex: m === active ? "0" : "-1",
      text: MODE_LABEL[m],
    }) as HTMLAnchorElement;
    links.push(link);
    bar.appendChild(link);
  }

  // Manual activation follows the APG tabs pattern: arrows move focus without launching another
  // cohort request; Enter/Space activates the focused mode. This keeps keyboard scanning instant
  // even when the newly selected entity mode has network latency.
  bar.addEventListener("keydown", (event) => {
    const current = links.indexOf(event.target as HTMLAnchorElement);
    if (current < 0) return;
    const last = links.length - 1;
    let next = -1;
    if (event.key === "ArrowRight") next = current >= last ? 0 : current + 1;
    else if (event.key === "ArrowLeft") next = current <= 0 ? last : current - 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    else if (event.key === " " || event.key === "Spacebar") {
      event.preventDefault();
      links[current].click();
      return;
    }
    if (next >= 0) {
      event.preventDefault();
      links.forEach((link, index) => { link.tabIndex = index === next ? 0 : -1; });
      links[next].focus();
    }
  });
  return bar;
}

/// Filter tokens — removable chips reflecting the cohort's filters (matching CohortFilter semantics).
/// Removing a chip that owns a query key navigates without it (scope otherwise preserved).
function filterTokens(
  cohort: CohortSpec,
  scope: CohortSpec,
  selection: CohortSpec | null,
  query: Record<string, string>,
  client: TareClient,
  context: WorkspaceContext | undefined,
  onSaveError: (error: unknown) => void
): HTMLElement | null {
  if (cohort.filters.length === 0) return null;
  const wrap = el("div", { class: "inv-filters", "aria-label": "Active filters" });
  for (const f of cohort.filters) {
    const { label, queryKey } = filterToken(f);
    const chip = el("span", { class: "inv-filter chip" }, [el("span", { text: label })]);
    if (context) {
      const current = context.analysis.get();
      const belongsToScope = scope.filters.some((candidate) => sameFilter(candidate, f));
      const nextScope = belongsToScope
        ? { ...scope, filters: withoutFirstFilter(scope.filters, f) }
        : scope;
      let nextSelection = selection
        ? { ...selection, filters: withoutFirstFilter(selection.filters, f) }
        : null;
      if (nextSelection && sameCohort(nextSelection, nextScope)) nextSelection = null;
      const nextState: AnalysisState = {
        ...current,
        workspace: "investigate",
        scope: nextScope,
        selection: nextSelection,
      };
      const fallbackHref = routeHash("investigate", undefined, {
        ...query,
        ...(queryKey ? { [queryKey]: "" } : { f: encodeFilters(nextScope.filters) }),
      });
      const navigation = analysisStateLink(
        nextState,
        query,
        fallbackHref,
        `Investigate without ${label}`,
        client,
        context,
        onSaveError
      );
      chip.appendChild(
        el(
          "a",
          {
            class: "inv-filter-x",
            href: navigation.href,
            "aria-label": `Remove filter ${label}`,
            title: navigation.title ?? "Remove filter",
            onClick: navigation.onClick,
          },
          [icon("close", { size: 12 })]
        )
      );
    } else if (queryKey) {
      chip.appendChild(
        el(
          "button",
          {
            class: "inv-filter-x",
            "aria-label": `Remove filter ${label}`,
            title: "Remove filter",
            onClick: () => setRouteQuery({ [queryKey]: "" }),
          },
          [icon("close", { size: 12 })]
        )
      );
    }
    wrap.appendChild(chip);
  }
  return wrap;
}

const MODE_NOUN: Record<Mode, [singular: string, plural: string]> = {
  runs: ["run", "runs"],
  sessions: ["session", "sessions"],
  templates: ["template", "templates"],
  steps: ["step", "steps"],
  time: ["day", "days"],
};

/// Put the otherwise-empty entity pane to work with context derived from the exact ranked rows. The
/// sum is deliberately labelled "listed spend" (not cohort total): some facet dimensions can be
/// multi-valued, so presenting their row sum as an authoritative reconciled total would over-claim.
function resultOverview(rows: ResultRow[], mode: Mode): HTMLElement {
  const listed = rows.reduce((sum, row) => sum + Math.max(0, row.dollars), 0);
  const largest = rows.reduce((max, row) => Math.max(max, row.dollars), 0);
  const largestShare = listed > 0 ? (largest / listed) * 100 : null;
  const largestShareLabel = largestShare == null
    ? "—"
    : largestShare > 0 && largestShare < 1
      ? "<1%"
      : `${Math.round(largestShare)}%`;
  const [singular, plural] = MODE_NOUN[mode];
  return el("section", { class: "inv-result-overview", "aria-labelledby": "inv-result-overview-h" }, [
    el("h2", { id: "inv-result-overview-h", class: "subhead sub", text: "Current results" }),
    el("dl", { class: "inv-result-facts" }, [
      el("div", {}, [
        el("dt", { text: "Results" }),
        el("dd", { class: "num", text: `${fmtTokens(rows.length)} ${rows.length === 1 ? singular : plural}` }),
      ]),
      el("div", {}, [
        el("dt", { text: "Listed spend" }),
        el("dd", { class: "num", text: fmtUsd(listed) }),
      ]),
      el("div", {}, [
        el("dt", { text: "Largest share" }),
        el("dd", { class: "num", text: largestShareLabel }),
      ]),
    ]),
  ]);
}

const QUICK_FACETS: ReadonlyArray<{
  dimension: Extract<CohortDimension, "model" | "provider">;
  title: string;
  queryKey: "model" | "provider";
}> = [
  { dimension: "model", title: "Models", queryKey: "model" },
  { dimension: "provider", title: "Providers", queryKey: "provider" },
];

function quickFacetGroup(
  spec: (typeof QUICK_FACETS)[number],
  rows: FacetRow[],
  query: Record<string, string>,
  scope: CohortSpec,
  selection: CohortSpec | null,
  client: TareClient,
  context: WorkspaceContext | undefined,
  onSaveError: (error: unknown) => void
): HTMLElement | null {
  const ranked = rows
    .filter((row) => row.value.length > 0 && row.selection_support > 0)
    .sort((a, b) => b.selection_micros - a.selection_micros)
    .slice(0, 4);
  if (ranked.length === 0) return null;
  const max = ranked.reduce((value, row) => Math.max(value, row.selection_micros), 0) || 1;
  const group = el("section", { class: "inv-quick-facet", "aria-label": `Filter by ${spec.dimension}` });
  const head = el("div", { class: "inv-quick-facet-head" }, [
    el("h3", { class: "caption", text: spec.title }),
  ]);
  const activeValue = selectedFacetValue(selection, spec.dimension);
  const navigationFor = (nextSelection: CohortSpec | null, fallbackHref: string, label: string): LinkNavigation => {
    if (!context) return { href: fallbackHref };
    const nextState: AnalysisState = {
      ...context.analysis.get(),
      workspace: "investigate",
      selection: nextSelection,
    };
    return analysisStateLink(nextState, query, fallbackHref, label, client, context, onSaveError);
  };
  if (activeValue) {
    const cleared = selectionWithFacet(selection ?? scope, spec.dimension, null);
    const nextSelection = sameCohort(cleared, scope) ? null : cleared;
    const navigation = navigationFor(
      nextSelection,
      routeHash("investigate", undefined, { ...query, [spec.queryKey]: "", sel: "" }),
      `Investigate without ${spec.title.toLowerCase()} filter`
    );
    head.appendChild(
      el("a", {
        class: "inv-facet-clear caption",
        href: navigation.href,
        title: navigation.title,
        onClick: navigation.onClick,
        text: "Clear",
      })
    );
  }
  group.appendChild(head);
  const list = el("ul", { class: "inv-facet-list" });
  for (const row of ranked) {
    const active = activeValue === row.value;
    const support = `${fmtTokens(row.selection_support)} run${row.selection_support === 1 ? "" : "s"}`;
    const nextSelection = selectionWithFacet(selection ?? scope, spec.dimension, row.value);
    const navigation = navigationFor(
      nextSelection,
      routeHash("investigate", undefined, { ...query, [spec.queryKey]: row.value, sel: "", investigation: "" }),
      `Investigate Selection A · ${spec.dimension} ${row.value}`
    );
    list.appendChild(
      el("li", {}, [
        el("a", {
          class: `inv-facet-row${active ? " active" : ""}`,
          href: navigation.href,
          title: navigation.title ?? `Set Selection A to ${spec.dimension} ${row.value}`,
          onClick: navigation.onClick,
          "aria-current": active ? "true" : undefined,
        }, [
          el("span", { class: "inv-facet-label", text: row.value }),
          el("span", { class: "inv-facet-spend num", text: fmtUsd(row.selection_micros) }),
          el("span", { class: "inv-facet-track", "aria-hidden": "true" }, [
            el("span", {
              class: "inv-facet-fill",
              style: `width:${Math.max(2, (row.selection_micros / max) * 100).toFixed(1)}%`,
            }),
          ]),
          el("span", { class: "inv-facet-support caption sub", text: support }),
        ]),
      ])
    );
  }
  group.appendChild(list);
  return group;
}

/// Load two bounded, high-value facets for the persistent filter pane. Failures stay local to this
/// supplementary navigation aid; ranked results and the inspector remain independently usable.
async function populateQuickFacets(
  host: HTMLElement,
  client: TareClient,
  scope: CohortSpec,
  selection: CohortSpec | null,
  query: Record<string, string>,
  context: WorkspaceContext | undefined
): Promise<void> {
  const active = selection ?? scope;
  const saveStatus = el("p", { class: "caption sub inv-facet-status", "aria-live": "polite" });
  const onSaveError = (error: unknown): void => {
    const detail = error instanceof Error && error.message ? ` ${error.message}` : "";
    saveStatus.textContent = `Couldn't save this large Selection A.${detail}`;
  };
  const groups = await Promise.all(
    QUICK_FACETS.map(async (spec) => {
      try {
        // Standard faceted-search behavior: retain every active constraint except this dimension so
        // alternatives remain visible and switching model/provider never requires a full reset.
        const faceted = selectionWithFacet(active, spec.dimension, null);
        const response = await client.facetCohort({ selection: faceted, baseline: scope, dimension: spec.dimension });
        return { spec, rows: response.data.rows, failed: false };
      } catch {
        return { spec, rows: [] as FacetRow[], failed: true };
      }
    })
  );
  const content = groups
    .map(({ spec, rows }) => quickFacetGroup(spec, rows, query, scope, selection, client, context, onSaveError))
    .filter((node): node is HTMLElement => node !== null);
  host.replaceChildren(
    el("h2", { class: "subhead sub", text: selection ? "Refine Selection A" : "Build Selection A" }),
    ...content
  );
  if (content.length === 0) {
    host.appendChild(
      el("p", {
        class: "caption sub",
        text: groups.some((group) => group.failed)
          ? "Model and provider breakdown is temporarily unavailable."
          : "No model or provider values are recorded in this scope.",
      })
    );
  }
  host.appendChild(saveStatus);
}

/// Ranked result table (ranked bars by default + sortable/keyboard columns). Row activation updates the
/// canonical selection by deep-linking to the entity's evidence (scope preserved).
function resultTable(rows: ResultRow[], mode: Mode): HTMLElement {
  const max = rows.reduce((m, r) => Math.max(m, r.dollars), 0) || 1;
  const cols: Array<Column<ResultRow>> = [
    {
      key: "bar",
      label: "",
      ariaLabel: "Relative spend",
      cell: (r) => {
        // Ranked bar in ink (never the brand accent, dataInkGuard). Decorative — value is the column.
        const bar = el("div", { class: "inv-bar", "aria-hidden": "true" });
        bar.appendChild(el("span", { class: "inv-bar-fill", style: `width:${((r.dollars / max) * 100).toFixed(1)}%` }));
        return bar;
      },
    },
    {
      key: "label",
      label: mode === "time" ? "Day" : MODE_LABEL[mode].replace(/s$/, ""),
      sortValue: (r) => r.label,
      cell: (r) => r.label,
    },
    { key: "spend", label: "Est. spend", numeric: true, sortValue: (r) => r.dollars, cell: (r) => fmtUsd(r.dollars) },
  ];
  if (rows.some((r) => r.support != null)) {
    cols.push({
      key: "support",
      label: "Runs",
      numeric: true,
      sortValue: (r) => r.support ?? 0,
      cell: (r) => (r.support != null ? fmtTokens(r.support) : "—"),
    });
  }
  const byKey = new Map(rows.map((r) => [r.key, r]));
  return dataTable(rows, cols, {
    rowKey: (r) => r.key,
    search: (r) => r.label,
    searchPlaceholder: "Filter these results…",
    initialSort: { key: "spend", dir: "desc" },
    rowHeight: 36, // bounded row windowing for large cohorts
    onActivate: (key) => {
      // Activating a row updates the canonical selection by navigating to its evidence deep-link
      // (already a scope-preserving canonical hash).
      const r = byKey.get(key);
      if (r) window.location.hash = r.href;
    },
  });
}

function cohortPeriod(cohort: CohortSpec): string {
  if (cohort.from && cohort.to) return cohort.from === cohort.to ? cohort.from : `${cohort.from}–${cohort.to}`;
  if (cohort.from) return `from ${cohort.from}`;
  if (cohort.to) return `through ${cohort.to}`;
  return "all captured dates";
}

/// Persistent context strip for the durable analysis state. Scope, Selection A, and Baseline B stay
/// named while the user drills from a cohort into a representative run and back; an unset comparison
/// is stated as such rather than disappearing. Sample counts are shown only when measured.
function analysisContext(
  scope: CohortSpec,
  selection: CohortSpec | null,
  baseline: BaselineSpec | null
): HTMLElement {
  const active = selection ?? scope;
  const baselineText = baseline
    ? `${baseline.label}${baseline.sampleCount != null && !baseline.label.includes(`${baseline.sampleCount} runs`) ? ` · ${baseline.sampleCount} runs` : ""}`
    : "Not selected";
  return el("div", {
    class: "inv-analysis-context",
    role: "group",
    "aria-label": "Analysis context",
  }, [
    el("span", {
      class: "inv-scope caption",
      text: `Scope · ${cohortPeriod(scope)} · ${scope.timezone} · ${metricLabel(scope.metric)} / ${normalizationLabel(scope.normalization)}`,
    }),
    el("span", {
      class: "inv-selection caption",
      text: selection
        ? `Selection A · ${active.filters.length} filter${active.filters.length === 1 ? "" : "s"} · ${cohortPeriod(active)}`
        : "Selection A · Whole scope",
    }),
    el("span", { class: "inv-baseline caption", text: `Baseline B · ${baselineText}` }),
  ]);
}

/// Render the Investigate workspace. Nested run/compare routes keep their evidence; the base route
/// gets the mode/search/filter/result workspace. `route.query.entity` picks the mode (default Runs).
export async function renderInvestigate(
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context?: WorkspaceContext
): Promise<void> {
  // Nested canonical routes — evidence targets. Run Profile replaces the legacy long-scroll
  // run document and loads only when this nested route is opened.
  if (route.segments[1] === "run" && route.segments[2]) {
    const { renderRunProfile } = await import("./runProfile.js");
    await renderRunProfile(root, client, route, context);
    return;
  }
  if (route.segments[1] === "compare") {
    await renderCompare(root, client, route, context);
    return;
  }

  const query = route.query ?? {};
  // Compatibility lenses now live as explicit children of Investigate instead of orphaned peer
  // routes. Lazy imports preserve their unique capability while the rail, breadcrumb, and URL stay
  // canonical. The common return action prevents a dead-end in desktop windows without browser chrome.
  const advancedLens =
    query.view === "units"
      ? { label: "Work units", load: () => import("../screens/units.js").then((m) => m.renderUnits) }
      : query.mode === "lineage"
        ? { label: "Prompt lineage", load: () => import("../screens/lineage.js").then((m) => m.renderLineage) }
        : query.mode === "distinguish"
          ? { label: "Configuration correlations", load: () => import("../screens/correlate.js").then((m) => m.renderCorrelate) }
          : null;
  if (advancedLens) {
    const host = el("div", { class: "investigate-advanced-host" });
    root.replaceChildren(
      el("section", { class: "investigate-advanced", "aria-label": advancedLens.label }, [
        el("header", { class: "investigate-advanced-head" }, [
          el("a", { class: "btn", href: routePath(["investigate"]), text: "← Back to Investigate" }),
          el("p", { class: "eyebrow", text: advancedLens.label }),
        ]),
        host,
      ])
    );
    try {
      const render = await advancedLens.load();
      await render(host, client);
    } catch (error) {
      host.replaceChildren(
        errorNode(`Couldn't load ${advancedLens.label}.`, error, {
          actions: [
            { label: "Retry", primary: true, run: () => renderInvestigate(root, client, route, context) },
            { label: "Back to Investigate", href: "#/investigate" },
          ],
        })
      );
    }
    return;
  }
  const routeMode = query.mode === "timeline"
    ? "time"
    : query.entity === "run"
      ? "runs"
      : query.entity === "step"
        ? "steps"
        : query.entity;
  const mode: Mode = (MODES as readonly string[]).includes(routeMode ?? "") ? (routeMode as Mode) : "runs";
  const shared = context?.analysis.get();
  const scope = shared?.scope ?? baseCohort(query);
  let selection = shared?.selection ?? null;
  // One-release compatibility for old `?model=`/`?provider=` links. In the shared workbench these
  // shortcut keys were previously ignored because the durable store won; promote them to the same
  // real Selection A as the canonical `sel=` path, then all new links serialize canonically.
  const legacyFilters = legacyQueryFilters(query);
  if (legacyFilters.length > 0 && context) {
    const promoted = applyLegacyFilters(selection ?? scope, legacyFilters);
    if (!selection || !sameCohort(selection, promoted)) context.analysis.setSelection(promoted);
    selection = promoted;
  }
  const cohort = selection ?? scope;

  const section = el("section", {
    class: "investigate",
    "aria-label": "Investigate",
    "data-adaptive-panes": "",
  }, [
    el("div", { class: "inv-head" }, [
      // Not an <h1>: the breadcrumb leaf is the page heading (main.ts setBreadcrumb), so a second
      // <h1> here gave every canonical workspace TWO h1s — and on Scenarios they even disagreed
      // ("Optimize" in the crumb, "Scenarios" here). Same class, so the visual treatment is
      // unchanged; only the heading semantics are fixed.
      el("p", { class: "inv-title", text: "Investigate" }),
      el("div", { class: "inv-head-actions" }, [
        analysisContext(scope, selection, shared?.baseline ?? null),
        el("button", {
          class: "btn pane-inspector-trigger",
          type: "button",
          "data-pane-inspector": "",
          "aria-controls": "investigate-inspector",
          "aria-expanded": "false",
          text: "Explain selection",
        }),
      ]),
    ]),
  ]);

  // Privacy-safe server search (identifiers only — never payload).
  const searchInput = el("input", {
    type: "search",
    class: "inv-search",
    placeholder: "Search IDs, labels, tags, notes…",
    "aria-label": "Search the cohort by identifier (never payload)",
    "aria-describedby": "investigate-search-privacy",
  }) as HTMLInputElement;
  const searchStatus = el("p", { class: "inv-search-status caption sub", "aria-live": "polite" });
  const resultOverviewHost = el("div", {
    class: "inv-result-overview-host",
    "aria-live": "polite",
  }, [el("p", { class: "caption sub", text: "Loading result summary…" })]);
  const quickFacetHost = el("div", {
    class: "inv-quick-facets",
    "aria-label": "Quick cohort filters",
  }, [
    el("h2", { class: "subhead sub", text: selection ? "Refine Selection A" : "Build Selection A" }),
    el("p", { class: "caption sub", text: "Loading model and provider breakdown…" }),
  ]);
  const filterActionStatus = el("p", {
    class: "caption sub inv-filter-action-status",
    "aria-live": "polite",
  });
  const entityPane = el("aside", {
    class: "inv-pane inv-entities",
    id: "investigate-entities",
    "data-pane": "entities",
    "aria-label": "Entities and filters",
  }, [
    modeSelector(mode, query),
    el("div", { class: "inv-searchbar" }, [searchInput, searchStatus]),
    // Keep the privacy boundary visible at every pane width. It used to live at the clipped end of
    // a placeholder, so wide-workbench users could not actually read the reassurance.
    el("p", {
      id: "investigate-search-privacy",
      class: "caption sub inv-search-privacy",
      text: "Identifiers only — prompt and response text are never searched.",
    }),
  ]);
  const tokens = filterTokens(
    cohort,
    scope,
    selection,
    query,
    client,
    context,
    (error) => {
      const detail = error instanceof Error && error.message ? ` ${error.message}` : "";
      filterActionStatus.textContent = `Couldn't update these filters.${detail}`;
    }
  );
  if (tokens) entityPane.appendChild(tokens);
  entityPane.appendChild(filterActionStatus);
  entityPane.appendChild(resultOverviewHost);
  entityPane.appendChild(quickFacetHost);

  const resultHost = el("div", {
    class: "inv-pane inv-results",
    id: "investigate-canvas",
    "data-pane": "canvas",
    role: "region",
    "aria-label": "Investigation canvas",
  });
  const inspectorHost = el("div", {
    class: "inv-pane inv-inspector-host",
    id: "investigate-inspector",
    "data-pane": "inspector",
    role: "region",
    "aria-label": "Inspector",
  });
  const entityResize = el("div", {
    class: "inv-resizer",
    "data-resize-pane": "entities",
    "aria-controls": "investigate-entities",
  });
  const inspectorResize = el("div", {
    class: "inv-resizer",
    "data-resize-pane": "inspector",
    "aria-controls": "investigate-inspector",
  });
  const paneNav = el("nav", {
    class: "inv-pane-nav",
    "data-pane-nav": "",
    "aria-label": "Pane navigation",
    hidden: true,
  }, [
    el("button", { class: "btn inv-pane-back", type: "button", "data-pane-back": "", text: "Back" }),
    el("span", { class: "inv-pane-title", "data-pane-title": "", "aria-live": "polite" }),
    el("button", {
      class: "btn inv-pane-forward",
      type: "button",
      "data-pane-forward": "",
      text: "Forward",
    }),
  ]);
  section.appendChild(paneNav);
  section.appendChild(el("div", { class: "inv-body" }, [
    entityPane,
    entityResize,
    resultHost,
    inspectorResize,
    inspectorHost,
  ]));
  root.replaceChildren(section);
  const initialPane = query.view === "facets"
    ? "inspector"
    : query.mode === "timeline" || query.entity
      ? "canvas"
      : undefined;
  installAdaptivePaneInteractions(section, {
    analysis: context?.analysis,
    // Legacy Segments links canonicalize to `view=facets`. Reveal the inspector that actually owns
    // those facet explanations instead of rendering an indistinguishable default Runs view. An
    // explicit entity/Timeline destination similarly opens Results on one-pane layouts; only the
    // bare Investigate entry begins at Filters.
    initialPane,
  });

  // The inspector explains ONE authoritative selection: the current cohort, against the whole-scope
  // baseline (same dates, filters dropped). Async so the ranked results paint first.
  void (async () => {
    try {
      const baseline = shared?.baseline?.cohort ?? scope;
      inspectorHost.replaceChildren(
        await renderInspector(client, cohort, baseline, query, {
          comparisonReady: Boolean(selection || shared?.baseline),
        })
      );
    } catch {
      /* inspector is supplementary — a failure must not blank the result panes */
    }
  })();

  if (mode === "time") {
    resultOverviewHost.replaceChildren(
      el("section", { class: "inv-result-overview", "aria-labelledby": "inv-result-overview-h" }, [
        el("h2", { id: "inv-result-overview-h", class: "subhead sub", text: "Current view" }),
        el("p", { class: "caption sub", text: `Daily spend · ${cohortPeriod(cohort)}` }),
      ])
    );
    void populateQuickFacets(quickFacetHost, client, scope, selection, query, context);
    try {
      const { renderInvestigateTimeline } = await import("./investigateTimeline.js");
      await renderInvestigateTimeline(
        resultHost,
        client,
        scope,
        selection,
        shared?.baseline ?? null,
        context,
        query
      );
    } catch (e) {
      resultHost.replaceChildren(
        errorNode("The Investigate timeline couldn't load.", e, {
          actions: [
            { label: "Retry", primary: true, run: () => renderInvestigate(root, client, route, context) },
            { label: "Show runs", href: "#/investigate?entity=runs" },
          ],
        })
      );
    }
    return;
  }

  let rows: ResultRow[];
  try {
    rows = await loadResults(client, mode, cohort, query);
  } catch (e) {
    resultOverviewHost.replaceChildren(
      el("p", { class: "caption sub", text: "Result summary is unavailable until the scope loads." })
    );
    resultHost.replaceChildren(
      errorNode("Investigate results couldn't load for this scope.", e, {
        actions: [
          { label: "Retry", primary: true, run: () => renderInvestigate(root, client, route, context) },
          { label: "Back to Pulse", href: "#/pulse" },
        ],
      })
    );
    return;
  }
  const renderRows = (rs: ResultRow[]): void => {
    resultHost.dataset.paneLabel = `Results (${rs.length})`;
    section.dispatchEvent(new Event("tare:pane-labelchange"));
    resultOverviewHost.replaceChildren(resultOverview(rs, mode));
    resultHost.replaceChildren(
      rs.length === 0 ? el("p", { class: "caption sub", text: "No entities match this scope." }) : resultTable(rs, mode)
    );
  };
  renderRows(rows);
  void populateQuickFacets(quickFacetHost, client, scope, selection, query, context);

  // Server search narrows the ranked set to matching identifiers (privacy-safe fields only, server-side
  // so it never scans payload). Debounced.
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  searchInput.addEventListener("input", () => {
    clearTimeout(searchTimer);
    const q = searchInput.value.trim();
    searchTimer = setTimeout(() => {
      void (async () => {
        if (!q) {
          searchStatus.textContent = "";
          renderRows(rows);
          return;
        }
        try {
          const res = await client.searchCohort({ cohort, query: q, fields: SEARCH_FIELDS, limit: 200 });
          const matched = new Set(
            res.data.entities.map((e) => (e.step_ordinal != null ? `${e.run_id}#${e.step_ordinal}` : e.run_id))
          );
          const narrowed = rows.filter((r) => matched.has(r.key) || matched.has(r.key.split("#")[0]));
          searchStatus.textContent = `${narrowed.length} match${narrowed.length === 1 ? "" : "es"}${res.data.truncated ? " (truncated)" : ""} — identifiers only`;
          renderRows(narrowed);
        } catch {
          searchStatus.textContent = "Search unavailable.";
        }
      })();
    }, 200);
  });
}
