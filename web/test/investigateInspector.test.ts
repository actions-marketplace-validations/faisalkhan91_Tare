// Investigate cohort inspector. Verifies the acceptance: numbers come from the
// cohort APIs, association is never called cause, the inspector reflects ONE authoritative selection,
// and representative/factor math is correct + accessible.

import { describe, it, expect } from "vitest";
import { rankFactors, representatives, renderInspector } from "../src/workspaces/investigateInspector.js";
import { fakeClient } from "./fakeClient.js";
import type { FacetRow, CohortEntitySummary, CohortSpec } from "../src/analysis/types.js";

const prov = {
  refreshed_at: "2026-07-15",
  scope: {} as CohortSpec,
  capture_sources: ["proxy"],
  coverage_status: "full",
  priced_token_share_pct: 98,
  component_fidelity: "component",
  pricing_edition: "2026.07",
  allocation_method: "component",
  value_class: "observed",
  assumptions: ["Prices are the effective-dated edition."],
} as never;

function facet(value: string, delta: number, lift: number, support = 5, micros = 3_000_000, missing = 0): FacetRow {
  return {
    value,
    selection_support: support,
    baseline_support: 3,
    selection_micros: micros,
    baseline_micros: 1_000_000,
    selection_support_share_pct: 60,
    baseline_support_share_pct: 30,
    selection_spend_share_pct: 70,
    baseline_spend_share_pct: 20,
    delta_support_share_points: delta,
    lift_ratio: lift,
    selection_missing_pct: missing,
    baseline_missing_pct: 0,
  };
}
function ent(run_id: string, micros: number): CohortEntitySummary {
  return { entity: { run_id }, matched_micros: micros, whole_entity_micros: micros, matched_step_count: 1 };
}

describe("inspector math (pure)", () => {
  it("rankFactors keeps only over-represented values, ranked by delta share points", () => {
    const rows = [facet("a", 30, 3), facet("b", 10, 1.5), facet("c", -5, 0.5)]; // c under-represented → dropped
    const f = rankFactors("model", rows);
    expect(f.map((x) => x.value)).toEqual(["a", "b"]);
    expect(f[0].lift).toBe(3);
    expect(f[0].dimension).toBe("model");
  });

  it("representatives picks largest, most-anomalous (furthest from mean), and median", () => {
    const es = [ent("r1", 10_000_000), ent("r2", 1_000_000), ent("r3", 900_000), ent("r4", 800_000), ent("r5", 700_000)];
    const r = representatives(es)!;
    expect(r.largest.entity.run_id).toBe("r1"); // max spend
    expect(r.mostAnomalous.entity.run_id).toBe("r1"); // furthest from the mean (the $10 outlier)
    expect(r.median.entity.run_id).toBe("r3"); // middle of 5 sorted desc
    expect(representatives([])).toBeNull();
  });
});

describe("renderInspector", () => {
  function client() {
    return fakeClient({
      resolveCohort: async () => ({
        data: { run_ids: ["r1", "r2"], run_count: 2, step_count: 4, total_micros: 9_000_000, entity_rows: [ent("r1", 6_000_000), ent("r2", 3_000_000)] },
        provenance: prov,
      }),
      facetCohort: async (req) => ({
        data: { dimension: req.dimension, rows: req.dimension === "model" ? [facet("gpt-4o", 40, 2.5, 5, 5_000_000, 10)] : [] },
        provenance: prov,
      }),
      compareCohort: async () => ({
        data: {
          selection: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
          baseline: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
          total_delta_micros: 4_000_000,
          volume_delta_micros: 2_500_000,
          size_delta_micros: 1_000_000,
          efficiency_delta_micros: 500_000,
          compatibility_warnings: [],
        },
        provenance: prov,
      }),
    });
  }
  const spec = {} as CohortSpec;

  it("shows distinguishing factors as ASSOCIATION, never called cause; numbers from the API", async () => {
    const insp = await renderInspector(client(), spec, spec);
    const factors = insp.querySelector(".insp-factors")!;
    expect(factors.textContent).toMatch(/association, not a proven cause/i);
    expect(insp.textContent!.toLowerCase()).not.toMatch(/\bcaused by\b|\bthe cause of\b/);
    // The API's facet value + lift surface verbatim.
    expect(insp.querySelector(".insp-factor-name")?.textContent).toBe("model = gpt-4o");
    expect(insp.querySelector(".insp-factor-metrics")?.textContent).toMatch(/lift 2\.5×/);
  });

  it("shows the volume/size/efficiency decomposition with a confounding caveat (aggregate)", async () => {
    const insp = await renderInspector(client(), spec, spec);
    const dl = insp.querySelector(".insp-decomp-dl")!;
    expect(dl.textContent).toMatch(/Volume/);
    expect(dl.textContent).toMatch(/\$2\.50/); // volume delta from the API
    expect(insp.querySelector(".insp-confound")?.textContent).toMatch(/confounded|approximate/i);
  });

  it("lists representative runs (largest/most-anomalous/median) from the resolved cohort", async () => {
    const insp = await renderInspector(client(), spec, spec, {
      from: "2026-07-10",
      sel: "selection-payload",
      base: "baseline-payload",
    });
    const reps = insp.querySelectorAll(".insp-reps .insp-rep");
    expect(reps.length).toBe(3);
    expect(insp.querySelector(".insp-reps")?.textContent).toMatch(/Largest/);
    expect(insp.querySelector(".insp-reps")?.textContent).toMatch(/Most anomalous/);
    const href = insp.querySelector(".insp-rep .insp-run")?.getAttribute("href") ?? "";
    expect(href).toMatch(/#\/investigate\/run\/r1/);
    expect(href).toContain("from=2026-07-10");
    expect(href).toContain("sel=selection-payload");
    expect(href).toContain("base=baseline-payload");
  });

  it("surfaces honest provenance (coverage/allocation/fidelity/priced share)", async () => {
    const insp = await renderInspector(client(), spec, spec);
    const p = insp.querySelector(".insp-prov")!;
    expect(p.textContent).toMatch(/Coverage: full/);
    expect(p.textContent).toMatch(/allocation: component/);
    expect(p.textContent).toMatch(/98% priced/);
  });

  it("does not present a whole-scope identity comparison as a page of meaningless zero deltas", async () => {
    const c = client();
    let facetCalls = 0;
    let compareCalls = 0;
    const originalFacet = c.facetCohort;
    const originalCompare = c.compareCohort;
    c.facetCohort = async (request) => {
      facetCalls += 1;
      return originalFacet(request);
    };
    c.compareCohort = async (request) => {
      compareCalls += 1;
      return originalCompare(request);
    };
    const insp = await renderInspector(c, spec, spec, {}, { comparisonReady: false });
    expect(insp.querySelector("h2")?.textContent).toBe("About this scope");
    expect(insp.querySelector(".insp-start")?.textContent).toMatch(/choose a model or provider filter/i);
    expect(insp.querySelector(".insp-factors")).toBeNull();
    expect(insp.querySelector(".insp-decomp")).toBeNull();
    expect(insp.querySelector(".insp-reps")).toBeTruthy();
    expect(facetCalls).toBe(0);
    expect(compareCalls).toBe(0);
  });

  it("is accessible: labelled inspector region + sectioned subheads", async () => {
    const insp = await renderInspector(client(), spec, spec);
    expect(insp.getAttribute("aria-label")).toBe("Cohort inspector");
    expect(insp.querySelectorAll("h3.subhead").length).toBeGreaterThanOrEqual(3);
  });
});
