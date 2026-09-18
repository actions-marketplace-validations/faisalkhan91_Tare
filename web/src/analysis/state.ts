// Calibrated Bench investigation-workspace state. This is LOCAL UI-only state, so its own fields
// are camelCase even though it embeds the
// snake_case wire DTOs (`CohortSpec`, `MatchRule`) verbatim. Durable selection/pin/comparison use
// `UiEntityRef` (below), NOT the cohort wire `CohortEntityRef` — the wire ref can only express
// run/step grain, but the workspace pins runs, steps, sessions, templates, and whole cohorts.
// `SavedInvestigationV2` persists `Omit<AnalysisState, "focus">`: durable state
// minus the transient focus that is never saved.

import type { CohortSpec, MatchRule } from "./types.js";

// ---- Baseline derivation ----
//
// A baseline is derived four ways. The COHORT it resolves is pure and honest here; the sample count
// is filled in by the caller after resolving (so the label never claims a count it hasn't measured).

const DAY_MS = 86_400_000;

function toEpochDay(date: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return null;
  const t = Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return Number.isNaN(t) ? null : Math.floor(t / DAY_MS);
}

function fromEpochDay(day: number): string {
  const d = new Date(day * DAY_MS);
  const y = d.getUTCFullYear();
  const mo = String(d.getUTCMonth() + 1).padStart(2, "0");
  const da = String(d.getUTCDate()).padStart(2, "0");
  return `${y}-${mo}-${da}`;
}

/// The immediately preceding equal-length calendar window `[from-len, from-1]` for a scope's
/// `[from, to]`, in the SAME timezone. `null` if the scope has no bounded window
/// (an unbounded scope has no well-defined prior window). Non-date settings are preserved by the
/// caller cloning the scope.
export function priorWindow(from: string, to: string): { from: string; to: string } | null {
  const a = toEpochDay(from);
  const b = toEpochDay(to);
  if (a === null || b === null || b < a) return null;
  const len = b - a + 1;
  return { from: fromEpochDay(a - len), to: fromEpochDay(a - 1) };
}

/// Honest baseline label: the rule name plus the measured sample count. Until the count is known the
/// label states the rule alone; it never invents a number. Once known, the sample count is included.
export function baselineLabel(kind: BaselineSpec["kind"], sampleCount?: number): string {
  const rule = {
    prior_window: "Prior window",
    rest_of_scope: "Rest of scope",
    pinned_run: "Pinned run",
    explicit_cohort: "Explicit cohort",
  }[kind];
  return sampleCount === undefined ? rule : `${rule} · ${sampleCount} runs`;
}

/// Derive the `prior_window` baseline for a bounded scope: the same cohort over the preceding
/// equal-length window (all non-date filters/pricing/metric/normalization preserved). `null`
/// when the scope is unbounded (no window to precede).
export function priorWindowBaseline(scope: CohortSpec, sampleCount?: number): BaselineSpec | null {
  if (!scope.from || !scope.to) return null;
  const w = priorWindow(scope.from, scope.to);
  if (!w) return null;
  return {
    kind: "prior_window",
    label: baselineLabel("prior_window", sampleCount),
    cohort: { ...scope, from: w.from, to: w.to },
    sampleCount,
  };
}

/// Derive the `pinned_run` baseline: the scope narrowed to a single run id. Compatibility
/// warnings (workload/pricing/fidelity) are surfaced by the caller after resolving.
export function pinnedRunBaseline(scope: CohortSpec, runId: string, sampleCount?: number): BaselineSpec {
  return {
    kind: "pinned_run",
    label: baselineLabel("pinned_run", sampleCount),
    cohort: { ...scope, filters: [{ op: "run_ids", ids: [runId] }] },
    sampleCount,
  };
}

/// Wrap a user-provided `explicit_cohort` baseline. It is NEVER silently rewritten when the scope
/// changes — the caller re-prompts on scope change instead.
export function explicitCohortBaseline(cohort: CohortSpec, sampleCount?: number): BaselineSpec {
  return {
    kind: "explicit_cohort",
    label: baselineLabel("explicit_cohort", sampleCount),
    cohort,
    sampleCount,
  };
}

/// A durable UI reference to a selected/pinned/compared entity. Local workspace state
/// (camelCase), DISTINCT from the cohort wire `CohortEntityRef { run_id, step_ordinal? }`: this
/// spans every Investigate entity grain, and `id` is an opaque UI-owned identifier (for a `step`,
/// `id` encodes both run and ordinal — see `stepRefId` in `investigation.ts`). `label` is an
/// optional display cache; it is not authoritative and may be recomputed.
export interface UiEntityRef {
  kind: "run" | "step" | "session" | "template" | "cohort";
  id: string;
  label?: string;
}

/// How a baseline cohort is derived for a comparison.
export interface BaselineSpec {
  kind: "prior_window" | "rest_of_scope" | "pinned_run" | "explicit_cohort";
  label: string;
  cohort: CohortSpec;
  sampleCount?: number;
}

/// Transient keyboard/pointer focus + highlight. This is the ONE part of AnalysisState that
/// is NEVER persisted to a saved investigation — it's ephemeral per-session interaction state.
export interface FocusState {
  pane: "rail" | "entities" | "canvas" | "inspector";
  highlighted: UiEntityRef | null;
  inspectorTab?: string;
  framePath?: string[];
  stepOrdinal?: number;
}

/// The full investigation-workspace state. `focus` is transient; everything else is durable
/// and is what a saved investigation captures.
export interface AnalysisState {
  workspace: "pulse" | "investigate" | "optimize";
  scope: CohortSpec;
  selection: CohortSpec | null;
  baseline: BaselineSpec | null;
  match: MatchRule;
  focus: FocusState;
  comparison: UiEntityRef[];
  pinned: UiEntityRef | null;
}

/// The durable slice of AnalysisState a saved investigation persists — everything except the
/// transient `focus`. A distinct named type so the omission is enforced at the type level.
export type DurableAnalysisState = Omit<AnalysisState, "focus">;
