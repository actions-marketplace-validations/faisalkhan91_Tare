import { describe, it, expect } from "vitest";
import {
  compareMatrix,
  rowIsFlat,
  cellFill,
  renderCompare,
  decompositionParts,
  causeDeltas,
} from "../src/screens/compare.js";
import { fakeClient, fakeProvenance } from "./fakeClient.js";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { parseHash } from "../src/ui/store.js";
import type { ReportDiff, RunStep } from "../src/client.js";
import type { CohortCompareRequest, CohortCompareResult } from "../src/analysis/types.js";
import type { WorkspaceContext } from "../src/shell/workbench.js";

describe("compareMatrix", () => {
  it("unions causes across runs (missing = 0) and sorts rows by total spend desc", () => {
    const m = compareMatrix(["a", "b"], {
      a: { "retry-loop": 3_000_000, "bloated-system-prompt": 500_000 },
      b: { "retry-loop": 1_000_000, "verbose-tool-output": 2_000_000 },
    });
    expect(m.map((r) => r.cause)).toEqual(["retry-loop", "verbose-tool-output", "bloated-system-prompt"]);
    // retry-loop row carries both runs' values, index-aligned.
    expect(m[0]).toMatchObject({ cause: "retry-loop", values: [3_000_000, 1_000_000], total: 4_000_000 });
    // verbose-tool-output is absent from run a -> 0.
    expect(m[1].values).toEqual([0, 2_000_000]);
  });
});

describe("rowIsFlat / cellFill", () => {
  it("treats within-epsilon rows as flat (neutral)", () => {
    expect(rowIsFlat([1_000_000, 1_000_500])).toBe(true); // within $0.001
    expect(rowIsFlat([1_000_000, 2_000_000])).toBe(false);
    expect(cellFill(5, 5, 5)).toBe("transparent");
  });
  it("colors cheapest with the cost-ok token and most-expensive with cost-high", () => {
    const cheap = cellFill(0, 0, 1_000_000);
    const dear = cellFill(1_000_000, 0, 1_000_000);
    // Theme-aware: token-derived fills via color-mix, not hardcoded rgba.
    expect(cheap).toContain("color-mix");
    expect(cheap).toContain("var(--cost-ok)");
    expect(dear).toContain("var(--cost-high)");
  });
});

describe("decompositionParts", () => {
  const make = (v: number, s: number, e: number, total: number) => ({
    selection: {} as never,
    baseline: {} as never,
    total_delta_micros: total,
    volume_delta_micros: v,
    size_delta_micros: s,
    efficiency_delta_micros: e,
    compatibility_warnings: [],
  });

  it("reports the three components summing EXACTLY to the total delta", () => {
    const p = decompositionParts(make(1_000_000, 2_000_000, 3_000_000, 6_000_000));
    expect(p.volume + p.size + p.efficiency).toBe(p.total);
    expect(p.sumsExactly).toBe(true);
    expect(p.reversals).toEqual([]);
  });

  it("flags a client-side sum mismatch instead of hiding it (contract regression is visible)", () => {
    const p = decompositionParts(make(1_000_000, 1_000_000, 1_000_000, 6_000_000));
    expect(p.sumsExactly).toBe(false);
  });

  it("marks components that move AGAINST the aggregate direction as reversals", () => {
    // Total rose (+), but efficiency fell (−): a countervailing move.
    const p = decompositionParts(make(5_000_000, 3_000_000, -2_000_000, 6_000_000));
    expect(p.reversals).toEqual(["efficiency"]);
    // A zero total has no direction, so nothing is a reversal.
    expect(decompositionParts(make(1_000_000, -1_000_000, 0, 0)).reversals).toEqual([]);
  });
});

describe("causeDeltas", () => {
  const diff = (rows: Array<[string, number, number]>) => ({
    pricing_version: "x",
    estimated: true,
    total_before: 0,
    total_after: 0,
    delta_micros: rows.reduce((acc, [, b, a]) => acc + (a - b), 0),
    rows: rows.map(([cause, micros_before, micros_after]) => ({
      cause,
      micros_before,
      micros_after,
      delta_micros: micros_after - micros_before,
    })),
  });

  it("ranks largest increases desc and largest decreases by magnitude, skipping flat causes", () => {
    const view = causeDeltas(diff([
      ["big-up", 1_000_000, 4_000_000], // +3
      ["small-up", 1_000_000, 1_500_000], // +0.5
      ["flat", 2_000_000, 2_000_000], // 0 → skipped
      ["big-down", 5_000_000, 1_000_000], // −4
    ]));
    expect(view.increases.map((r) => r.cause)).toEqual(["big-up", "small-up"]);
    expect(view.decreases.map((r) => r.cause)).toEqual(["big-down"]);
    expect(view.increases.every((r) => r.delta_micros > 0)).toBe(true);
  });

  it("flags causes moving opposite the run's aggregate delta direction", () => {
    // Aggregate delta is negative (−0.5); the +3 increase moves against it.
    const view = causeDeltas(diff([
      ["up", 1_000_000, 4_000_000], // +3
      ["down", 6_000_000, 2_500_000], // −3.5
    ]));
    expect(view.reversals.map((r) => r.cause)).toEqual(["up"]);
  });
});

describe("renderCompare", () => {
  function clientWith(perRun: Record<string, Record<string, number>>) {
    return fakeClient({
      diff: async (a: string): Promise<ReportDiff> => ({
        pricing_version: "x",
        estimated: true,
        total_before: 0,
        total_after: 0,
        delta_micros: 0,
        rows: Object.entries(perRun[a] ?? {}).map(([cause, micros]) => ({
          cause,
          detail: "",
          micros_before: micros,
          micros_after: micros,
          delta_micros: 0,
        })),
      }),
    });
  }

  it("prompts to pick runs when fewer than two are selected", async () => {
    location.hash = "#/compare";
    const root = document.createElement("div");
    await renderCompare(root, fakeClient());
    expect(root.querySelector(".empty-state")).toBeTruthy();
  });

  it("renders a cause × run matrix and a Differences-only toggle", async () => {
    location.hash = "#/compare?runs=a,b";
    const root = document.createElement("div");
    await renderCompare(
      root,
      clientWith({
        a: { "retry-loop": 3_000_000, shared: 1_000_000 },
        b: { "retry-loop": 1_000_000, shared: 1_000_000 },
      })
    );
    // One column per run + the cause column.
    // Lens subtitle interpolates the run count (regression guard: no bare placeholder).
    const lens = root.querySelector(".lens")?.textContent ?? "";
    expect(lens).toContain("2 runs compared side by side");
    expect(lens).not.toMatch(/\bN runs\b/);
    expect(root.querySelector(".compare-matrix")?.parentElement?.classList.contains("table-scroll")).toBe(true);
    const headers = Array.from(root.querySelectorAll("thead th")).map((h) => h.textContent);
    expect(headers).toContain("Cause");
    expect(root.querySelectorAll(".compare-matrix tbody tr").length).toBe(2); // retry-loop + shared
    // Differences-only hides the identical 'shared' row.
    const toggle = root.querySelector('input[type="checkbox"]') as HTMLInputElement;
    toggle.checked = true;
    toggle.dispatchEvent(new Event("change"));
    expect(root.querySelectorAll(".compare-matrix tbody tr").length).toBe(1);
    expect(root.textContent).toContain("retry-loop");
    location.hash = "";
  });

  it("renders the fixed-baseline summary matrix from the cohort comparison API", async () => {
    location.hash = "#/investigate/compare?runs=base,candidate";
    const initial = initialAnalysisState("investigate");
    initial.scope = {
      ...initial.scope,
      metric: "tokens",
      normalization: "per_run",
      filters: [
        { op: "eq", dimension: "provider", value: "anthropic" },
        { op: "run_ids", ids: ["stale"] },
      ],
    };
    initial.match = { kind: "workload_key", key: "nightly" };
    const context: WorkspaceContext = { analysis: createAnalysisStore(initial) };
    const requests: CohortCompareRequest[] = [];
    const result: CohortCompareResult = {
      selection: {
        run_ids: ["candidate"], run_count: 2, step_count: 5, total_micros: 15_000_000, entity_rows: [],
      },
      baseline: {
        run_ids: ["base"], run_count: 3, step_count: 7, total_micros: 10_000_000, entity_rows: [],
      },
      total_delta_micros: 5_000_000,
      total_delta_pct: 50,
      volume_delta_micros: 1_000_000,
      size_delta_micros: 2_000_000,
      efficiency_delta_micros: 2_000_000,
      compatibility_warnings: ["workload-key match is partial"],
    };
    const client = clientWith({ base: {}, candidate: {} });
    client.compareCohort = async (req) => {
      requests.push(req);
      return { data: result, provenance: fakeProvenance(req.selection) };
    };
    const root = document.createElement("div");
    await renderCompare(root, client, parseHash(location.hash), context);

    expect(requests).toHaveLength(1);
    expect(requests[0].match).toEqual({ kind: "workload_key", key: "nightly" });
    expect(requests[0].baseline.filters).toEqual([
      { op: "eq", dimension: "provider", value: "anthropic" },
      { op: "run_ids", ids: ["base"] },
    ]);
    expect(requests[0].selection.filters).toEqual([
      { op: "eq", dimension: "provider", value: "anthropic" },
      { op: "run_ids", ids: ["candidate"] },
    ]);
    expect(root.querySelector(".compare-chip.baseline")?.textContent).toContain("Baseline B");
    expect(root.querySelector(".compare-chip.baseline")?.textContent).toContain("base");
    const summary = root.querySelector(".compare-summary-matrix")?.textContent ?? "";
    expect(summary).toContain("3 runs");
    expect(summary).toContain("2 runs");
    expect(summary).toContain("$10.00");
    expect(summary).toContain("$15.00");
    expect(summary).toContain("+$5.00");
    expect(summary).toContain("50%");
    expect(summary).toContain("1.50×");
    expect(root.querySelector(".compare-warning")?.textContent).toContain("workload-key match is partial");
    expect(root.textContent).toContain("Match rule");
    expect(root.textContent).toContain("Estimated spend · absolute");
    expect(root.textContent).toContain("tokens / per run was not applied");
    location.hash = "";
  });

  it("renders the volume/size/efficiency decomposition summing to total, distinct from the cause report diff", async () => {
    location.hash = "#/investigate/compare?runs=base,candidate";
    const context: WorkspaceContext = { analysis: createAnalysisStore(initialAnalysisState("investigate")) };
    const result: CohortCompareResult = {
      selection: { run_ids: ["candidate"], run_count: 1, step_count: 5, total_micros: 15_000_000, entity_rows: [] },
      baseline: { run_ids: ["base"], run_count: 1, step_count: 7, total_micros: 10_000_000, entity_rows: [] },
      total_delta_micros: 5_000_000,
      total_delta_pct: 50,
      volume_delta_micros: 1_000_000,
      size_delta_micros: 6_000_000,
      efficiency_delta_micros: -2_000_000, // moves against the +total → a reversal
      compatibility_warnings: [],
    };
    const client = fakeClient({
      compareCohort: async (req: CohortCompareRequest) => ({ data: result, provenance: fakeProvenance(req.selection) }),
      diff: async (): Promise<ReportDiff> => ({
        pricing_version: "x",
        estimated: true,
        total_before: 10_000_000,
        total_after: 15_000_000,
        delta_micros: 5_000_000,
        rows: [
          { cause: "bigger-context", micros_before: 2_000_000, micros_after: 9_000_000, delta_micros: 7_000_000 },
          { cause: "cache-reuse", micros_before: 3_000_000, micros_after: 1_000_000, delta_micros: -2_000_000 },
        ],
      }),
    });
    const root = document.createElement("div");
    await renderCompare(root, client, parseHash(location.hash), context);

    // Cohort decomposition: three components + total, summing exactly, with explicit direction + units.
    const decomp = root.querySelector(".compare-decomposition")!;
    expect(decomp).toBeTruthy();
    expect(decomp.querySelector('[data-component="volume"] .dollars')?.textContent).toBe("+$1.00");
    expect(decomp.querySelector('[data-component="size"] .dollars')?.textContent).toBe("+$6.00");
    expect(decomp.querySelector('[data-component="efficiency"] .dollars')?.textContent).toBe("-$2.00");
    expect(decomp.querySelector('[data-metric="total"] .dollars')?.textContent).toBe("+$5.00");
    expect(decomp.querySelector('[data-sums-exactly="true"]')).toBeTruthy();
    // Efficiency moved against the +total → flagged as a reversal; units + direction are explicit.
    expect(decomp.querySelector('[data-component="efficiency"][data-reversal="true"]')).toBeTruthy();
    expect(decomp.textContent).toContain("estimated spend");
    expect(decomp.querySelector('[data-component="size"]')?.textContent).toContain("costlier");
    expect(decomp.querySelector('[data-component="efficiency"]')?.textContent).toContain("cheaper");

    // Cause report diff is a SEPARATE section (a different lens), ranked increases/decreases.
    const cause = root.querySelector(".compare-cause-diff")!;
    expect(cause).toBeTruthy();
    expect(cause === decomp).toBe(false); // kept distinct, never merged
    expect(cause.querySelector('[data-rank="increase"] .cause')?.textContent).toBe("bigger-context");
    expect(cause.querySelector('[data-rank="decrease"] .cause')?.textContent).toBe("cache-reuse");
    expect(cause.querySelector('[data-rank="increase"] .dollars')?.textContent).toBe("+$7.00");
    location.hash = "";
  });

  it("decomposes each candidate against the previous one in previous-reference mode", async () => {
    location.hash = "#/investigate/compare?runs=base,c1,c2&reference=previous";
    const context: WorkspaceContext = { analysis: createAnalysisStore(initialAnalysisState("investigate")) };
    const runIdsOf = (filters: CohortCompareRequest["baseline"]["filters"]): string[] =>
      filters.flatMap((f) => (f.op === "run_ids" ? f.ids : []));
    const baselineRunIds: string[] = [];
    const client = fakeClient({
      compareCohort: async (req: CohortCompareRequest) => {
        baselineRunIds.push(runIdsOf(req.baseline.filters).join(""));
        return {
          data: {
            selection: { run_ids: runIdsOf(req.selection.filters), run_count: 1, step_count: 1, total_micros: 1_000_000, entity_rows: [] },
            baseline: { run_ids: runIdsOf(req.baseline.filters), run_count: 1, step_count: 1, total_micros: 500_000, entity_rows: [] },
            total_delta_micros: 500_000,
            total_delta_pct: 100,
            volume_delta_micros: 200_000,
            size_delta_micros: 200_000,
            efficiency_delta_micros: 100_000,
            compatibility_warnings: [],
          } as CohortCompareResult,
          provenance: fakeProvenance(req.selection),
        };
      },
    });
    const root = document.createElement("div");
    await renderCompare(root, client, parseHash(location.hash), context);

    // Previous-mode decomposition re-fetches with previous-candidate baselines (c2 decomposed vs c1).
    expect(baselineRunIds).toContain("c1");
    // The section labels the previous-candidate reference explicitly.
    expect(root.querySelector(".compare-decomposition")?.textContent).toContain("Previous ·");
    location.hash = "";
  });

  it("enables the structural flame diff only for a resolved single-run pair", async () => {
    const context: WorkspaceContext = { analysis: createAnalysisStore(initialAnalysisState("investigate")) };
    const oneRun: CohortCompareResult = {
      selection: { run_ids: ["cand"], run_count: 1, step_count: 3, total_micros: 15_000_000, entity_rows: [] },
      baseline: { run_ids: ["base"], run_count: 1, step_count: 3, total_micros: 10_000_000, entity_rows: [] },
      total_delta_micros: 5_000_000,
      total_delta_pct: 50,
      volume_delta_micros: 0,
      size_delta_micros: 0,
      efficiency_delta_micros: 5_000_000,
      compatibility_warnings: [],
    };
    const flameModel = {
      run_a: "base", run_b: "cand", normalized: false,
      total_a_micros: 10_000_000, total_b_micros: 15_000_000,
      root: { name: "root", tokens_a: 0, tokens_b: 0, micros_a: 10_000_000, micros_b: 15_000_000, delta_micros: 5_000_000, share_a_bps: 10_000, share_b_bps: 10_000, delta_bps: 0, children: [
        { name: "tools", tokens_a: 0, tokens_b: 0, micros_a: 4_000_000, micros_b: 9_000_000, delta_micros: 5_000_000, share_a_bps: 4_000, share_b_bps: 6_000, delta_bps: 2_000, children: [] },
      ] },
    };
    let flameCalls = 0;
    const client = fakeClient({
      compareCohort: async (req: CohortCompareRequest) => ({ data: oneRun, provenance: fakeProvenance(req.selection) }),
      flameDiff: async () => { flameCalls += 1; return flameModel; },
    });

    // Single run each side: the pair auto-resolves, so the structural flame diff is present.
    location.hash = "#/investigate/compare?runs=base,cand";
    const single = document.createElement("div");
    await renderCompare(single, client, parseHash(location.hash), context);
    expect(single.querySelector(".flame-diff")).toBeTruthy();
    expect(single.querySelector('.flame-diff tr[data-path="0"]')?.textContent).toContain("costlier");
    expect(flameCalls).toBe(1);

    // Multi-run cohorts WITHOUT an explicit representative choice: no structural flame diff, no fetch.
    flameCalls = 0;
    const multi: CohortCompareResult = {
      ...oneRun,
      selection: { run_ids: ["cand", "cand2"], run_count: 2, step_count: 6, total_micros: 15_000_000, entity_rows: [] },
      baseline: { run_ids: ["base", "base2"], run_count: 2, step_count: 6, total_micros: 10_000_000, entity_rows: [] },
    };
    const multiClient = fakeClient({
      compareCohort: async (req: CohortCompareRequest) => ({ data: multi, provenance: fakeProvenance(req.selection) }),
      flameDiff: async () => { flameCalls += 1; return flameModel; },
    });
    location.hash = "#/investigate/compare?runs=base,cand";
    const many = document.createElement("div");
    await renderCompare(many, multiClient, parseHash(location.hash), context);
    expect(many.querySelector(".flame-diff")).toBeNull();
    expect(flameCalls).toBe(0);
    location.hash = "";
  });

  it("keeps Baseline B fixed while candidate reorder and removal update durable order", async () => {
    location.hash = "#/investigate/compare?runs=base,b,c";
    const context: WorkspaceContext = {
      analysis: createAnalysisStore(initialAnalysisState("investigate")),
    };
    const client = clientWith({ base: {}, b: {}, c: {} });
    const root = document.createElement("div");
    await renderCompare(root, client, parseHash(location.hash), context);

    (root.querySelector('[aria-label="Move candidate c left"]') as HTMLButtonElement).click();
    expect(parseHash(location.hash).query?.runs).toBe("base,c,b");
    expect(context.analysis.get().baseline?.cohort.filters).toContainEqual({ op: "run_ids", ids: ["base"] });
    expect(context.analysis.get().comparison.map((ref) => ref.id)).toEqual(["c", "b"]);

    await renderCompare(root, client, parseHash(location.hash), context);
    const headers = Array.from(root.querySelectorAll(".compare-summary-matrix thead th")).map((h) => h.textContent);
    expect(headers).toEqual(["Metric", "Baseline B · base", "Candidate · c", "Candidate · b"]);
    (root.querySelector('[aria-label="Remove candidate c"]') as HTMLButtonElement).click();
    expect(parseHash(location.hash).query?.runs).toBe("base,b");
    expect(context.analysis.get().baseline?.cohort.filters).toContainEqual({ op: "run_ids", ids: ["base"] });
    expect(context.analysis.get().comparison.map((ref) => ref.id)).toEqual(["b"]);
    location.hash = "";
  });

  it("integrates captured quality provenance, workload trial groups, unmatched rows, and an explicit pair picker", async () => {
    location.hash = "#/investigate/compare?runs=base,a,b,c";
    const initial = initialAnalysisState("investigate");
    initial.scope = { ...initial.scope, normalization: "per_run" };
    const context: WorkspaceContext = { analysis: createAnalysisStore(initial) };
    let flameDiffCalls = 0;
    const resultFor = (runId: string): CohortCompareResult => ({
      selection: {
        run_ids: [runId], run_count: 1, step_count: 1,
        total_micros: runId === "a" ? 12_000_000 : 8_000_000, entity_rows: [],
      },
      baseline: {
        run_ids: ["base"], run_count: 1, step_count: 1, total_micros: 10_000_000, entity_rows: [],
      },
      total_delta_micros: runId === "a" ? 2_000_000 : -2_000_000,
      total_delta_pct: runId === "a" ? 20 : -20,
      volume_delta_micros: 0,
      size_delta_micros: 0,
      efficiency_delta_micros: runId === "a" ? 2_000_000 : -2_000_000,
      compatibility_warnings: runId === "b" ? ["workload-key match is partial"] : [],
    });
    const facetRow = (
      value: string,
      selectionSupport: number,
      baselineSupport: number,
      selectionMissingPct: number,
      baselineMissingPct: number
    ) => ({
      value,
      selection_support: selectionSupport,
      baseline_support: baselineSupport,
      selection_micros: 0,
      baseline_micros: 0,
      selection_support_share_pct: selectionSupport ? 100 : 0,
      baseline_support_share_pct: baselineSupport ? 100 : 0,
      selection_spend_share_pct: 0,
      baseline_spend_share_pct: 0,
      delta_support_share_points: 0,
      lift_ratio: baselineSupport ? selectionSupport / baselineSupport : undefined,
      selection_missing_pct: selectionMissingPct,
      baseline_missing_pct: baselineMissingPct,
    });
    const client = clientWith({ base: {}, a: {}, b: {}, c: {} });
    client.compareCohort = async (req) => {
      const ids = req.selection.filters.find((filter) => filter.op === "run_ids");
      const runId = ids?.op === "run_ids" ? ids.ids[0] : "a";
      return { data: resultFor(runId), provenance: fakeProvenance(req.selection) };
    };
    client.facetCohort = async (req) => {
      const ids = req.selection.filters.find((filter) => filter.op === "run_ids");
      const runId = ids?.op === "run_ids" ? ids.ids[0] : "a";
      return {
        data: {
          dimension: "workload_key",
          rows: runId === "a"
            ? [facetRow("nightly", 1, 1, 0, 0)]
            : [facetRow("nightly", 0, 1, 100, 0)],
        },
        provenance: fakeProvenance(req.selection),
      };
    };
    client.frontier = async () => ({
      points: [
        { run_id: "base", cost_micros: 10_000_000, quality: 80, quality_source: "ci", on_frontier: true },
        { run_id: "a", cost_micros: 12_000_000, quality: 91, quality_source: "header", on_frontier: true },
        { run_id: "b", cost_micros: 8_000_000, on_frontier: true },
        // c is deliberately absent: Compare must retain it as an unmatched captured-evidence row.
      ],
      has_quality: true,
      pricing_version: "2026-07-01",
      estimated: true,
    });
    client.flameDiff = async (...args) => {
      flameDiffCalls += 1;
      return fakeClient().flameDiff(...args);
    };

    const root = document.createElement("div");
    await renderCompare(root, client, parseHash(location.hash), context);

    const frontier = root.querySelector(".compare-frontier")?.textContent ?? "";
    expect(frontier).toContain("base");
    expect(frontier).toContain("80 · CI · user-supplied");
    expect(frontier).toContain("91 · capture header · user-supplied");
    expect(frontier).toContain("b");
    expect(frontier).toContain("Unscored · not quality-ranked");
    expect(frontier).toContain("c");
    expect(frontier).toContain("Not returned · row retained");
    expect(frontier).toContain("whole-run estimates");

    const trials = root.querySelector(".compare-trial-groups")?.textContent ?? "";
    expect(trials).toContain("nightly");
    expect(trials).toContain("Matched workload key");
    expect(trials).toContain("Baseline-only · unmatched");
    expect(trials).toContain("Missing workload key");
    expect(trials).toContain("Unmatched · missing key");
    expect(root.querySelector(".compare-summary")?.getAttribute("data-normalization")).toBe("per_run");
    expect(root.textContent).toContain("per run was not applied");

    const baselinePicker = root.querySelector('[aria-label="Baseline representative run"]') as HTMLSelectElement;
    const candidatePicker = root.querySelector('[aria-label="Candidate representative run"]') as HTMLSelectElement;
    expect(baselinePicker.value).toBe("base");
    expect(candidatePicker.value).toBe(""); // multiple candidates require an explicit choice
    candidatePicker.value = "b";
    candidatePicker.dispatchEvent(new Event("change"));
    expect(parseHash(location.hash).query).toMatchObject({
      pair_baseline: "base",
      pair_candidate: "b",
    });
    expect(flameDiffCalls).toBe(0);
    // No aggregate flame tree is synthesized for a multi-run cohort without an explicit pair: the
    // structural flame-diff section (.flame-diff) is absent until one is chosen.
    expect(root.querySelector(".flame-diff")).toBeNull();
    expect(root.querySelector<HTMLAnchorElement>(".compare-optimize-link")?.getAttribute("href"))
      .toContain("#/optimize?view=scenarios");
    location.hash = "";
  });

  it("renders zero-baseline percent and multiplier as unavailable", async () => {
    location.hash = "#/investigate/compare?runs=zero,candidate";
    const client = clientWith({ zero: {}, candidate: {} });
    client.compareCohort = async (req) => ({
      data: {
        selection: { run_ids: ["candidate"], run_count: 1, step_count: 1, total_micros: 1_000_000, entity_rows: [] },
        baseline: { run_ids: ["zero"], run_count: 1, step_count: 1, total_micros: 0, entity_rows: [] },
        total_delta_micros: 1_000_000,
        volume_delta_micros: 0,
        size_delta_micros: 0,
        efficiency_delta_micros: 1_000_000,
        compatibility_warnings: [],
      },
      provenance: fakeProvenance(req.selection),
    });
    const root = document.createElement("div");
    await renderCompare(root, client);
    const summary = root.querySelector(".compare-summary-matrix")?.textContent ?? "";
    expect(summary.match(/Unavailable/g)?.length).toBeGreaterThanOrEqual(2);
    expect(summary).not.toContain("Infinity");
    expect(summary).not.toContain("0.00×");
    location.hash = "";
  });

  it("uses Selection A and durable Baseline B in Cohorts mode", async () => {
    location.hash = "#/investigate/compare?mode=cohorts";
    const initial = initialAnalysisState("investigate");
    initial.selection = { ...initial.scope, filters: [{ op: "eq", dimension: "model", value: "candidate" }] };
    initial.baseline = {
      kind: "explicit_cohort",
      label: "Release baseline",
      cohort: { ...initial.scope, filters: [{ op: "eq", dimension: "model", value: "base" }] },
      sampleCount: 4,
    };
    const context: WorkspaceContext = { analysis: createAnalysisStore(initial) };
    let request: CohortCompareRequest | undefined;
    const client = fakeClient({
      compareCohort: async (req) => {
        request = req;
        return {
          data: {
            selection: { run_ids: ["candidate"], run_count: 2, step_count: 2, total_micros: 2, entity_rows: [] },
            baseline: { run_ids: ["base"], run_count: 4, step_count: 4, total_micros: 1, entity_rows: [] },
            total_delta_micros: 1,
            total_delta_pct: 100,
            volume_delta_micros: 0,
            size_delta_micros: 0,
            efficiency_delta_micros: 1,
            compatibility_warnings: [],
          },
          provenance: fakeProvenance(req.selection),
        };
      },
    });
    const root = document.createElement("div");
    await renderCompare(root, client, parseHash(location.hash), context);
    expect(request?.selection).toEqual(initial.selection);
    expect(request?.baseline).toEqual(initial.baseline.cohort);
    expect(root.querySelector(".compare-chip.baseline")?.textContent).toContain("Release baseline");
    expect(root.textContent).toContain("Selection A");
    location.hash = "";
  });

  it("compares ordered template versions and keeps Scenarios mode non-synthetic", async () => {
    location.hash = "#/investigate/compare?mode=versions&versions=v1,v2";
    const requests: CohortCompareRequest[] = [];
    const client = fakeClient({
      compareCohort: async (req) => {
        requests.push(req);
        return {
          data: {
            selection: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
            baseline: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
            total_delta_micros: 0,
            volume_delta_micros: 0,
            size_delta_micros: 0,
            efficiency_delta_micros: 0,
            compatibility_warnings: [],
          },
          provenance: fakeProvenance(req.selection),
        };
      },
    });
    const root = document.createElement("div");
    await renderCompare(root, client);
    expect(requests).toHaveLength(1);
    expect(requests[0].baseline.filters).toContainEqual({ op: "eq", dimension: "template", value: "v1" });
    expect(requests[0].selection.filters).toContainEqual({ op: "eq", dimension: "template", value: "v2" });

    location.hash = "#/investigate/compare?mode=scenarios";
    await renderCompare(root, client);
    expect(requests).toHaveLength(1);
    expect(root.textContent).toContain("Scenarios are evaluated in Optimize");
    expect(root.textContent).toContain("No comparison values were synthesized");
    expect(root.querySelector(".compare-summary-matrix")).toBeNull();
    location.hash = "";
  });

  it("renders a parallel-coordinates panel across cost dimensions", async () => {
    location.hash = "#/compare?runs=a,b";
    const step = (over: Partial<RunStep>): RunStep => ({
      ordinal: 1,
      provider: "anthropic",
      model: "m",
      fresh_input: 500,
      cache_read: 500,
      cache_write: 0,
      output: 100,
      reasoning: 0,
      tokens: 1100,
      micros: 1_000_000,
      stop_reason: "end_turn",
      ...over,
    });
    const client = fakeClient({
      diff: async (): Promise<ReportDiff> => ({
        pricing_version: "x",
        estimated: true,
        total_before: 0,
        total_after: 0,
        delta_micros: 0,
        rows: [],
      }),
      runSteps: async (id: string) =>
        id === "a" ? [step({ micros: 1_000_000 })] : [step({ micros: 4_000_000, fresh_input: 900, cache_read: 100 })],
    });
    const root = document.createElement("div");
    await renderCompare(root, client);
    expect(root.textContent).toContain("Cost dimensions");
    const svg = root.querySelector("svg.parcoords") as SVGElement;
    expect(svg).toBeTruthy();
    expect(svg.querySelectorAll(".pc-axis").length).toBe(5); // tokens, output%, cache-miss%, $/1M, spend
    expect(svg.querySelectorAll(".pc-line").length).toBe(2);
    // b is the pricier run -> cost-high line.
    expect(svg.querySelector(".pc-line.cost-high")).toBeTruthy();
    location.hash = "";
  });

  it("renders a cost-vs-tokens scatter colored by $/1M-tok", async () => {
    location.hash = "#/compare?runs=a,b";
    const client = fakeClient({
      diff: async (): Promise<ReportDiff> => ({
        pricing_version: "x",
        estimated: true,
        total_before: 0,
        total_after: 0,
        delta_micros: 0,
        rows: [],
      }),
      // a is cheap per token, b is 4× the rate -> b should be cost-high.
      runStatus: async (run_id: string) => ({
        run_id,
        steps: 1,
        top_cause: null,
        micros: run_id === "a" ? 1_000_000 : 4_000_000,
        tokens: 1_000_000,
      }),
    });
    const root = document.createElement("div");
    await renderCompare(root, client);
    expect(root.textContent).toContain("Cost vs tokens");
    const svg = root.querySelector("svg.scatter") as SVGElement;
    expect(svg).toBeTruthy();
    expect(svg.querySelectorAll(".scatter-dot").length).toBe(2);
    // The pricier-per-token run is flagged cost-high.
    expect(svg.querySelector(".scatter-dot.cost-high")).toBeTruthy();
    location.hash = "";
  });
});
