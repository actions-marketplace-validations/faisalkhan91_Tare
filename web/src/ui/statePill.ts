// Shared lifecycle state pill: a fixed, scannable vocabulary so a user can glance down a
// list of sessions or runs and see what needs attention. Severity drives the color token; a glyph
// is always paired with the text, so state never relies on hue alone (accessibility).

import { el } from "./el.js";

/// Live-session lifecycle (recency-derived). Runs use a separate vocabulary below.
export type LiveState = "working" | "idle" | "ended";
/// Run lifecycle. Priority when several apply: errored > over-cap > unpriced > settled.
export type RunState = "settled" | "over-cap" | "unpriced" | "errored";
/// Local-service lifecycle: the embedded capture daemon as a managed service.
export type ServiceState = "running" | "degraded" | "stopped";
export type PillState = LiveState | RunState | ServiceState;

interface PillSpec {
  label: string;
  glyph: string;
  /// Maps to a `.state-<severity>` color token: ok | warn | error | neutral.
  severity: "ok" | "warn" | "error" | "neutral";
  /// One-line definition of what the state MEANS — used as the pill's tooltip and the
  /// inline legend, so a bare "Settled"/"Unpriced" is decodable. Never a mere repeat of the label.
  desc: string;
}

// Sentence-case labels (see DESIGN_GUIDE.md): these render as-is (no CSS text-transform on
// .state-pill), so the source case is what the user sees — leading-lowercase would be the anti-pattern.
const SPECS: Record<PillState, PillSpec> = {
  // live
  working: { label: "Working", glyph: "●", severity: "ok", desc: "A step arrived recently; in progress." },
  idle: { label: "Idle", glyph: "◐", severity: "warn", desc: "No recent activity, but not yet ended." },
  ended: { label: "Ended", glyph: "○", severity: "neutral", desc: "No activity for a while; treated as finished." },
  // run
  settled: { label: "Settled", glyph: "✓", severity: "ok", desc: "Priced and finalized." },
  "over-cap": { label: "Over cap", glyph: "▲", severity: "error", desc: "Spend exceeded this run's cap." },
  unpriced: { label: "Unpriced", glyph: "?", severity: "warn", desc: "No price found for this model, so spend can't be estimated." },
  errored: { label: "Errored", glyph: "✕", severity: "error", desc: "The run reported an error." },
  // local service
  running: { label: "Running", glyph: "●", severity: "ok", desc: "The capture service is up and listening." },
  degraded: { label: "Degraded", glyph: "◐", severity: "warn", desc: "Running but not fully healthy." },
  stopped: { label: "Stopped", glyph: "○", severity: "neutral", desc: "The capture service is not running." },
};

/// Derive a run's lifecycle state from its status flags + a per-run spend cap (micro-USD; 0/omitted
/// = no cap). Priority: errored > over-cap > unpriced > settled.
export function runState(
  s: { micros: number; unpriced?: boolean; errored?: boolean },
  capMicros = 0
): RunState {
  if (s.errored) return "errored";
  if (capMicros > 0 && s.micros > capMicros) return "over-cap";
  if (s.unpriced) return "unpriced";
  return "settled";
}

/// Render a state pill. `title` overrides the default tooltip, which defaults to the state's DEFINITION
/// — not a repeat of the visible label, so hover actually explains "Settled"/"Unpriced".
export function statePill(state: PillState, title?: string): HTMLElement {
  const spec = SPECS[state] ?? SPECS.settled;
  return el("span", { class: `state-pill state-${spec.severity}`, title: title ?? spec.desc }, [
    el("span", { class: "state-glyph", text: spec.glyph }),
    el("span", { text: spec.label }),
  ]);
}
