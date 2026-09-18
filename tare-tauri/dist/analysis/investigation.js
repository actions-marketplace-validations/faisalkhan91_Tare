// Durable saved investigations. These are the persisted, named
// investigations — stored in SQLite via the client API (the cross-transport source of truth). The
// The old saved-view/local-layout migration bridge has been removed: SQLite
// (`SavedInvestigationV2`) is now the SOLE durable source. Active focus is NEVER persisted
// (it's omitted from the DTO's `state`).
/// Build a v2 investigation from active analysis state. The transient focus is removed here at the
/// persistence boundary rather than relying on callers to remember the omission. `now` and
/// `id` are caller-supplied so tests stay deterministic.
export function investigationFromState(id, label, state, now) {
    const { focus: _focus, ...durable } = state;
    return {
        id,
        label,
        version: 2,
        state: durable,
        created_at: now,
        updated_at: now,
    };
}
/// An empty cohort scope — the honest default when no real selection is set. (All fields explicit so
/// it hashes identically to a hand-built empty spec.)
function emptyScope() {
    return {
        from: null,
        to: null,
        timezone: "UTC",
        entity: "run",
        filters: [],
        pricing: { mode: "effective_dated" },
        metric: "spend_micros",
        normalization: "absolute",
        outcome_denominator: null,
    };
}
/// Map a route segment to a Calibrated Bench workspace using the route contract: the three-workspace
/// collapse (`live|overview → pulse`, `runs|sessions|trends → investigate`, `optimize|experiments →
/// optimize`). Unknown routes default to `investigate` (the general browser).
export function routeToWorkspace(route) {
    switch (route) {
        case "live":
        case "overview":
        case "pulse":
            return "pulse";
        case "optimize":
        case "experiments":
            return "optimize";
        default:
            return "investigate";
    }
}
/// Map the user's global baseline/pinned-run PREFERENCES into a durable-state fragment
/// — used to seed the ACTIVE investigation. Pure: a baseline is produced ONLY when a baseline run is
/// actually set (no invention); the first pinned run becomes the pinned entity. Returns an empty
/// object when neither is set.
export function migratePrefsToState(baselineRun, pinnedRuns) {
    const out = {};
    if (baselineRun) {
        out.baseline = {
            kind: "pinned_run",
            label: `Run ${baselineRun}`,
            cohort: { ...emptyScope(), filters: [{ op: "run_ids", ids: [baselineRun] }] },
        };
    }
    if (pinnedRuns.length > 0) {
        out.pinned = runRef(pinnedRuns[0]);
    }
    return out;
}
// ---- UI entity references ----
//
// A step spans a (run, ordinal) pair, but `UiEntityRef.id` is a single opaque string. Encode a step
// as `<run_id>#<ordinal>`; runs/sessions/templates/cohorts use their natural id directly.
const STEP_DELIM = "#";
/// A durable run reference.
export function runRef(runId, label) {
    return { kind: "run", id: runId, label: label ?? `Run ${runId}` };
}
/// A durable step reference; `id` encodes the owning run and the step ordinal.
export function stepRef(runId, ordinal, label) {
    return {
        kind: "step",
        id: `${runId}${STEP_DELIM}${ordinal}`,
        label: label ?? `Run ${runId} · step ${ordinal}`,
    };
}
/// Normalize a persisted entity reference to a `UiEntityRef`, migrating the legacy shape in which
/// durable UI refs were stored as the cohort wire `CohortEntityRef { run_id, step_ordinal? }`.
/// IDEMPOTENT: an already-`UiEntityRef` value passes through unchanged, so
/// re-running the migration is safe. Returns `null` for an unrecognizable value (dropped by the
/// caller rather than corrupting state).
export function normalizeUiEntityRef(raw) {
    if (!raw || typeof raw !== "object")
        return null;
    const r = raw;
    // Already a UiEntityRef.
    if (typeof r.kind === "string" && typeof r.id === "string") {
        const ref = { kind: r.kind, id: r.id };
        if (typeof r.label === "string")
            ref.label = r.label;
        return ref;
    }
    // Legacy cohort wire ref: { run_id, step_ordinal? } -> run/step UiEntityRef.
    if (typeof r.run_id === "string") {
        return typeof r.step_ordinal === "number"
            ? stepRef(r.run_id, r.step_ordinal)
            : runRef(r.run_id);
    }
    return null;
}
/// Idempotently migrate a persisted investigation's durable entity references (`comparison`,
/// `pinned`) from the legacy wire shape to `UiEntityRef`. Returns a new record;
/// unrecognizable comparison entries are dropped and an unrecognizable pin becomes `null`. All other
/// fields (id/label/scope/selection/baseline/match/columns/timestamps) are preserved
/// verbatim so cohort transport and metadata stay byte-identical.
export function upgradeInvestigationEntityRefs(inv) {
    const comparison = Array.isArray(inv.state.comparison)
        ? inv.state.comparison
            .map(normalizeUiEntityRef)
            .filter((r) => r !== null)
        : [];
    const pinned = inv.state.pinned != null ? normalizeUiEntityRef(inv.state.pinned) : null;
    return { ...inv, state: { ...inv.state, comparison, pinned } };
}
