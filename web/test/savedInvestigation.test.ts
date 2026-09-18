// Saved-investigation persistence and entity-reference migration. SQLite is the sole durable
// source; only current route/preferences mapping and legacy entity-reference migration remain.

import { describe, it, expect, vi, afterEach } from "vitest";
import {
  migratePrefsToState,
  routeToWorkspace,
  runRef,
  stepRef,
  normalizeUiEntityRef,
  upgradeInvestigationEntityRefs,
  investigationFromState,
  type SavedInvestigationV2,
} from "../src/analysis/investigation.js";
import { initialAnalysisState } from "../src/analysis/store.js";
import { fakeClient } from "./fakeClient.js";
import { createHttpClient } from "../src/httpClient.js";

const NOW = "2026-07-10T00:00:00Z";

describe("route + prefs mapping", () => {
  it("route table collapses the legacy screens", () => {
    expect(routeToWorkspace("live")).toBe("pulse");
    expect(routeToWorkspace("sessions")).toBe("investigate");
    expect(routeToWorkspace("experiments")).toBe("optimize");
    expect(routeToWorkspace("something-unknown")).toBe("investigate");
  });

  it("maps baseline/pin prefs into state only when actually set (no invention)", () => {
    expect(migratePrefsToState(null, [])).toEqual({});
    const mapped = migratePrefsToState("run-x", ["run-p", "run-q"]);
    expect(mapped.pinned).toEqual({ kind: "run", id: "run-p", label: "Run run-p" });
    expect(mapped.baseline?.kind).toBe("pinned_run");
    expect(mapped.baseline?.cohort.filters).toEqual([{ op: "run_ids", ids: ["run-x"] }]);
  });
});

describe("UiEntityRef construction + legacy wire-ref migration", () => {
  it("encodes run/step refs", () => {
    expect(runRef("run-a")).toEqual({ kind: "run", id: "run-a", label: "Run run-a" });
    const s = stepRef("run-a", 3);
    expect(s).toEqual({ kind: "step", id: "run-a#3", label: "Run run-a · step 3" });
    expect(stepRef("a#b", 7).id).toBe("a#b#7");
  });

  it("normalizes the legacy cohort wire ref shape, and is idempotent on UiEntityRef", () => {
    // Legacy persisted shape: durable UI refs stored as CohortEntityRef { run_id, step_ordinal? }.
    expect(normalizeUiEntityRef({ run_id: "r1" })).toEqual({
      kind: "run",
      id: "r1",
      label: "Run r1",
    });
    expect(normalizeUiEntityRef({ run_id: "r1", step_ordinal: 2 })).toEqual({
      kind: "step",
      id: "r1#2",
      label: "Run r1 · step 2",
    });
    // Already a UiEntityRef -> passes through unchanged (idempotent).
    const ui = { kind: "template" as const, id: "sys-hash", label: "Planner" };
    expect(normalizeUiEntityRef(ui)).toEqual(ui);
    expect(normalizeUiEntityRef(normalizeUiEntityRef({ run_id: "r1" }))).toEqual({
      kind: "run",
      id: "r1",
      label: "Run r1",
    });
    // Garbage is dropped.
    expect(normalizeUiEntityRef(null)).toBeNull();
    expect(normalizeUiEntityRef({ foo: "bar" })).toBeNull();
  });

  it("migrates a persisted investigation's comparison/pinned; preserves everything else", () => {
    const legacy = {
      id: "inv-1",
      title: "Old view",
      version: 2 as const,
      state: {
        workspace: "investigate",
        scope: { timezone: "UTC", entity: "run", filters: [], pricing: { mode: "effective_dated" }, metric: "spend_micros", normalization: "absolute" },
        selection: null,
        baseline: null,
        match: { kind: "aggregate_only" },
        // Legacy wire-shaped references persisted by older builds:
        comparison: [{ run_id: "r1" }, { run_id: "r2", step_ordinal: 5 }, { bogus: true }],
        pinned: { run_id: "r3" },
      },
      columns: { runs: ["a", "b"] },
      created_at: "2026-07-01T00:00:00Z",
      updated_at: "2026-07-02T00:00:00Z",
    } as unknown as SavedInvestigationV2;

    const up = upgradeInvestigationEntityRefs(legacy);
    expect(up.state.comparison).toEqual([
      { kind: "run", id: "r1", label: "Run r1" },
      { kind: "step", id: "r2#5", label: "Run r2 · step 5" },
    ]); // bogus entry dropped
    expect(up.state.pinned).toEqual({ kind: "run", id: "r3", label: "Run r3" });
    // Untouched fields preserved verbatim (transport + metadata byte-stable).
    expect(up.id).toBe("inv-1");
    expect(up.columns).toEqual({ runs: ["a", "b"] });
    expect(up.state.scope).toEqual(legacy.state.scope);
    expect(up.created_at).toBe("2026-07-01T00:00:00Z");
    // Idempotent: re-running yields an equal record.
    expect(upgradeInvestigationEntityRefs(up)).toEqual(up);
  });
});

describe("investigation transport migrates legacy refs on load", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("HTTP listInvestigations normalizes persisted legacy wire-shaped refs", async () => {
    const stored = [
      {
        id: "inv-1",
        title: "t",
        version: 2,
        state: {
          workspace: "investigate",
          scope: { timezone: "UTC", entity: "run", filters: [], pricing: { mode: "effective_dated" }, metric: "spend_micros", normalization: "absolute" },
          selection: null,
          baseline: null,
          match: { kind: "aggregate_only" },
          comparison: [{ run_id: "r1" }],
          pinned: { run_id: "r2", step_ordinal: 4 },
        },
        created_at: "2026-07-01T00:00:00Z",
        updated_at: "2026-07-01T00:00:00Z",
      },
    ];
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify(stored)));
    const c = createHttpClient("http://127.0.0.1:8788");
    const rows = await c.listInvestigations();
    expect(rows[0].state.comparison).toEqual([{ kind: "run", id: "r1", label: "Run r1" }]);
    expect(rows[0].state.pinned).toEqual({ kind: "step", id: "r2#4", label: "Run r2 · step 4" });
  });
});

describe("saved-investigation persistence round-trip", () => {
  it("constructs a durable v2 record without transient focus", () => {
    const state = initialAnalysisState("investigate");
    state.focus = { pane: "inspector", highlighted: { kind: "run", id: "r1" } };
    state.selection = { ...state.scope, filters: [{ op: "run_ids", ids: ["r1"] }] };
    const saved = investigationFromState("inv-focus", "Focus omitted", state, NOW);
    expect(saved.version).toBe(2);
    expect(saved.state.selection).toEqual(state.selection);
    expect("focus" in saved.state).toBe(false);
    expect(saved.created_at).toBe(NOW);
    expect(saved.updated_at).toBe(NOW);
  });

  it("upserts, lists newest-first, and deletes via the client", async () => {
    const c = fakeClient();
    // Hand-built v2 records with distinct updated_at so ordering is observable. Only id/version/
    // timestamps matter to the store; the durable `state` shape is exercised elsewhere.
    const mk = (id: string, label: string, updated: string): SavedInvestigationV2 =>
      ({
        id,
        label,
        version: 2,
        state: { workspace: "investigate" },
        created_at: NOW,
        updated_at: updated,
      }) as unknown as SavedInvestigationV2;
    await c.saveInvestigation(mk("a", "A", "2026-07-10T01:00:00Z"));
    await c.saveInvestigation(mk("b", "B", "2026-07-10T02:00:00Z"));

    const list = await c.listInvestigations();
    expect(list.map((i) => i.id)).toEqual(["b", "a"]); // newest-updated first
    await c.deleteInvestigation("b");
    expect((await c.listInvestigations()).map((i) => i.id)).toEqual(["a"]);
  });
});
