// Calibrated Bench AnalysisState store. A tiny framework-free observable
// holding the workspace analysis state. DURABLE state — scope (incl. metric/normalization/pricing/
// timezone), selection, baseline, match, comparison, pinned — survives workspace navigation; the
// transient `focus` is reset on every workspace change and is NEVER serialized to the URL or a saved
// investigation. Scope, selection, baseline, metric, and normalization survive workspace changes;
// focus does not survive application restart. The URL codec (serialize.ts)
// carries the shareable slice; over the 1,800-char budget the caller saves the investigation and
// links it by id rather than truncating.

import {
  baselineLabel,
  explicitCohortBaseline,
  type AnalysisState,
  type DurableAnalysisState,
  type FocusState,
  type UiEntityRef,
} from "./state.js";
import type { CohortSpec } from "./types.js";
import {
  decodeAnalysisQuery,
  encodeAnalysisQuery,
  MAX_HASH_LEN,
  overflowsHashBudget,
  type AnalysisUrlState,
} from "./serialize.js";
import { routePath } from "../ui/store.js";

/// The default transient focus. A fresh focus on every workspace entry.
export function defaultFocus(): FocusState {
  return { pane: "canvas", highlighted: null };
}

export type AnalysisListener = (state: AnalysisState) => void;

export interface AnalysisStore {
  get(): AnalysisState;
  subscribe(listener: AnalysisListener): () => void;
  set(patch: Partial<AnalysisState>): void;
  /// Enter a workspace: durable state is untouched and transient focus is reset.
  navigateWorkspace(workspace: AnalysisState["workspace"]): void;
  setScope(scope: CohortSpec): void;
  setSelection(selection: CohortSpec | null): void;
  setBaseline(baseline: AnalysisState["baseline"]): void;
  setPinned(pinned: UiEntityRef | null): void;
  setComparison(comparison: UiEntityRef[]): void;
  setFocus(patch: Partial<FocusState>): void;
  clearFocus(): void;
  /// The persistable slice (everything except transient focus).
  durable(): DurableAnalysisState;
}

/// Honest boot state for the shared workbench store. Every CohortSpec field is explicit so the
/// first Pulse render, a later URL hydration, and a saved investigation all canonicalize the same
/// way. `focus` is the only transient field.
export function initialAnalysisState(
  workspace: AnalysisState["workspace"] = "pulse"
): AnalysisState {
  return {
    workspace,
    scope: {
      from: null,
      to: null,
      timezone: "UTC",
      entity: "run",
      filters: [],
      pricing: { mode: "effective_dated" },
      metric: "spend_micros",
      normalization: "absolute",
      outcome_denominator: null,
    },
    selection: null,
    baseline: null,
    match: { kind: "aggregate_only" },
    focus: defaultFocus(),
    comparison: [],
    pinned: null,
  };
}

/// Create a store seeded with `initial`.
export function createAnalysisStore(initial: AnalysisState): AnalysisStore {
  let state = initial;
  const listeners = new Set<AnalysisListener>();
  const emit = () => {
    for (const l of [...listeners]) l(state);
  };
  return {
    get: () => state,
    subscribe(listener) {
      listeners.add(listener);
      listener(state);
      return () => {
        listeners.delete(listener);
      };
    },
    set(patch) {
      state = { ...state, ...patch };
      emit();
    },
    navigateWorkspace(workspace) {
      // Durable state survives; only the transient focus resets.
      state = { ...state, workspace, focus: defaultFocus() };
      emit();
    },
    setScope(scope) {
      state = { ...state, scope };
      emit();
    },
    setSelection(selection) {
      state = { ...state, selection };
      emit();
    },
    setBaseline(baseline) {
      state = { ...state, baseline };
      emit();
    },
    setPinned(pinned) {
      state = { ...state, pinned };
      emit();
    },
    setComparison(comparison) {
      state = { ...state, comparison };
      emit();
    },
    setFocus(patch) {
      state = { ...state, focus: { ...state.focus, ...patch } };
      emit();
    },
    clearFocus() {
      state = { ...state, focus: defaultFocus() };
      emit();
    },
    durable() {
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      const { focus: _focus, ...rest } = state;
      return rest;
    },
  };
}

/// Project the durable analysis state onto the URL codec's shareable shape. The scope's
/// own settings (dates/tz/entity/metric/normalization/pricing/outcome) become individual params, its
/// filters become `f`, the selection becomes `sel`, and the baseline's cohort becomes `base`.
export function analysisToUrlState(state: AnalysisState): AnalysisUrlState {
  const s = state.scope;
  const url: AnalysisUrlState = {
    tz: s.timezone,
    entity: s.entity,
    metric: s.metric,
    norm: s.normalization,
    pricing: s.pricing,
    filters: s.filters,
    view: undefined,
  };
  if (s.from) url.from = s.from;
  if (s.to) url.to = s.to;
  if (s.outcome_denominator) url.outcome = s.outcome_denominator;
  if (state.selection) url.selection = state.selection;
  if (state.baseline) {
    url.baseline = state.baseline.cohort;
    url.baselineKind = state.baseline.kind;
    url.baselineSampleCount = state.baseline.sampleCount;
  }
  return url;
}

/// Merge a decoded URL state back into a scope. Only the fields the URL carries are
/// overwritten; unset fields keep the base scope's value so a partial link degrades sensibly.
export function scopeFromUrlState(base: CohortSpec, url: AnalysisUrlState): CohortSpec {
  return {
    ...base,
    from: url.from ?? base.from ?? null,
    to: url.to ?? base.to ?? null,
    timezone: url.tz ?? base.timezone,
    entity: url.entity ?? base.entity,
    metric: url.metric ?? base.metric,
    normalization: url.norm ?? base.normalization,
    pricing: url.pricing ?? base.pricing,
    outcome_denominator: url.outcome ?? base.outcome_denominator ?? null,
    filters: url.filters ?? base.filters,
  };
}

/// Hydrate analysis state from a URL query. Throws `AnalysisUrlError` on any invalid/unknown
/// value so the caller can surface it — never silently drops or guesses.
export function hydrateFromQuery(base: AnalysisState, query: Record<string, string>): AnalysisState {
  const url = decodeAnalysisQuery(query);
  return {
    ...base,
    scope: scopeFromUrlState(base.scope, url),
    // `undefined` means the route did not speak about Selection A, so durable navigation keeps it.
    // `null` is the codec's explicit whole-scope marker and must clear it.
    selection: url.selection !== undefined ? url.selection : base.selection,
    // Compatibility: old links carry only `base=` and remain explicit cohorts. New links add the
    // rule/count companions so a measured prior-window baseline does not lose its identity on reload.
    baseline: url.baseline
      ? url.baselineKind
        ? {
            kind: url.baselineKind,
            label: baselineLabel(url.baselineKind, url.baselineSampleCount),
            cohort: url.baseline,
            sampleCount: url.baselineSampleCount,
          }
        : explicitCohortBaseline(url.baseline)
      : base.baseline,
  };
}

/// The outcome of serializing analysis state to a shareable hash. Over the 1,800-char budget
/// the state cannot be inlined: the caller must save the investigation and navigate by its id.
export type AnalysisHash =
  | { kind: "inline"; hash: string }
  | { kind: "requires_save"; reason: string };

/// Assemble the canonical analysis hash for `segments` + state, or report that it overflows the
/// 1,800-char budget. Never truncates. When an `investigationId` is supplied it always
/// inlines just that id (the compact form a saved investigation uses). `routeQuery` carries
/// destination-specific UI state such as Timeline mode alongside the analysis state.
export function serializeAnalysisHash(
  segments: string[],
  state: AnalysisState,
  investigationId?: string,
  routeQuery?: Record<string, string>
): AnalysisHash {
  if (investigationId) {
    return {
      kind: "inline",
      hash: routePath(segments, { ...(routeQuery ?? {}), investigation: investigationId }),
    };
  }
  const query = { ...encodeAnalysisQuery(analysisToUrlState(state)), ...(routeQuery ?? {}) };
  const hash = routePath(segments, query);
  if (overflowsHashBudget(hash)) {
    return {
      kind: "requires_save",
      reason: `analysis hash is ${hash.length} chars (> ${MAX_HASH_LEN}); save the investigation and link it by id`,
    };
  }
  return { kind: "inline", hash };
}
