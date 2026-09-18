// Shared lifecycle state pill: a fixed, scannable vocabulary so a user can glance down a
// list of sessions or runs and see what needs attention. Severity drives the color token; a glyph
// is always paired with the text, so state never relies on hue alone (accessibility).
import { el } from "./el.js";
// Sentence-case labels (see DESIGN_GUIDE.md): these render as-is (no CSS text-transform on
// .state-pill), so the source case is what the user sees — leading-lowercase would be the anti-pattern.
const SPECS = {
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
export function runState(s, capMicros = 0) {
    if (s.errored)
        return "errored";
    if (capMicros > 0 && s.micros > capMicros)
        return "over-cap";
    if (s.unpriced)
        return "unpriced";
    return "settled";
}
/// Render a state pill. `title` overrides the default tooltip, which defaults to the state's DEFINITION
/// — not a repeat of the visible label, so hover actually explains "Settled"/"Unpriced".
export function statePill(state, title) {
    const spec = SPECS[state] ?? SPECS.settled;
    return el("span", { class: `state-pill state-${spec.severity}`, title: title ?? spec.desc }, [
        el("span", { class: "state-glyph", text: spec.glyph }),
        el("span", { text: spec.label }),
    ]);
}
