import { describe, expect, it, vi } from "vitest";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import type {
  AnalysisResponse,
  CohortResolveResult,
  CohortSpec,
  ExperimentRequest,
  ExperimentResult,
} from "../src/analysis/types.js";
import { parseHash } from "../src/ui/store.js";
import { renderOptimize } from "../src/workspaces/optimize.js";
import { fakeClient } from "./fakeClient.js";

const SCOPE: CohortSpec = {
  from: "2026-07-01",
  to: "2026-07-14",
  timezone: "America/Los_Angeles",
  entity: "run",
  filters: [],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
  outcome_denominator: null,
};

const SELECTION: CohortSpec = {
  ...SCOPE,
  filters: [{ op: "eq", dimension: "workload_key", value: "nightly" }],
};

const RESOLVED: AnalysisResponse<CohortResolveResult> = {
  data: {
    run_ids: ["run-a", "run-b", "run-c"],
    run_count: 3,
    step_count: 12,
    total_micros: 10_000_000,
    entity_rows: [],
  },
  provenance: {
    refreshed_at: "2026-07-16T12:00:00Z",
    scope: SELECTION,
    capture_sources: ["otel-event", "proxy"],
    coverage_status: "partial",
    priced_token_share_pct: 87.5,
    component_fidelity: "cost_class",
    pricing_edition: {
      version: "2026-07-01",
      effective_date: "2026-07-01",
      mode: "effective",
    },
    allocation_method: "provider_counts",
    value_class: "derived",
    assumptions: ["One legacy run uses its immutable capture-date bucket."],
  },
};

const EXPERIMENT_RESULT: ExperimentResult = {
  cells: [
    {
      coords: [
        { axis: "model", value: "a-model" },
        { axis: "pricing_snapshot", value: "2026-06-01" },
        { axis: "cache_strategy", value: true },
      ],
      label: ["model=a-model", "pricing=2026-06-01", "cache=decache"],
      cost_micros: 4_000_000,
      approximate: true,
    },
    {
      coords: [
        { axis: "model", value: null },
        { axis: "pricing_snapshot", value: null },
        { axis: "cache_strategy", value: false },
      ],
      label: ["model=as-captured", "pricing=as-captured", "cache=as-captured"],
      cost_micros: 10_000_000,
      approximate: false,
    },
  ],
  pareto: [0],
  baseline_micros: 10_000_000,
  best_micros: 4_000_000,
  best_saving_micros: 6_000_000,
  pricing_version: "2026-07-01",
  estimated: true,
  approximate: true,
};

function context() {
  const analysis = createAnalysisStore(initialAnalysisState("optimize"));
  analysis.setScope(SCOPE);
  analysis.setSelection(SELECTION);
  return { analysis };
}

function scenarioClient(overrides: Parameters<typeof fakeClient>[0] = {}) {
  return fakeClient({
    resolveCohort: async () => RESOLVED,
    advise: async () => [
      {
        model: "claude-opus-4-8",
        system_tokens: 20_000,
        sends: 5,
        uncached_micros: 5_000_000,
        cached_5m_micros: 2_000_000,
        cached_1h_micros: 2_500_000,
        save_5m_micros: 3_000_000,
        save_1h_micros: 2_500_000,
        recommend: "5m",
        breakeven_reads: 2,
      },
    ],
    whatif: async (crossProvider) => ({
      baseline_micros: 10_000_000,
      estimated: true,
      approximate: true,
      recommendations: [
        {
          to_provider: crossProvider ? "openai" : "anthropic",
          to_model: crossProvider ? "gpt-5-mini" : "claude-haiku-4-5",
          total_after_micros: 4_000_000,
          delta_micros: -6_000_000,
          approximate_tokenizer: true,
          approximate_cross_provider: crossProvider,
        },
      ],
    }),
    frontier: async () => ({
      points: [
        { run_id: "run-b", cost_micros: 8_000_000, quality: 92, on_frontier: true },
        { run_id: "run-a", cost_micros: 5_000_000, quality: 86, on_frontier: true },
        { run_id: "run-c", cost_micros: 9_000_000, quality: 70, on_frontier: false },
      ],
      has_quality: true,
      pricing_version: "2026-07-01",
      estimated: true,
    }),
    ...overrides,
  });
}

describe("Optimize scenario workbench", () => {
  it("unifies model/cache/frontier inputs while keeping scope, provenance, and compatibility honest", async () => {
    const root = document.createElement("div");
    await renderOptimize(
      root,
      scenarioClient(),
      parseHash("#/optimize?from=2026-07-01&to=2026-07-14&view=scenarios"),
      context()
    );

    expect(root.querySelector(".optimize-scenarios")).toBeTruthy();
    expect(root.textContent).toContain("Model what-if");
    expect(root.textContent).toContain("Prompt-cache advice");
    expect(root.textContent).toContain("Cost–quality frontier");
    expect(root.textContent).toContain("Build a counterfactual grid");
    expect(root.textContent).toContain("Whole captured store, not the active Selection");
    expect(root.textContent).toContain("Selection A provenance");
    expect(root.textContent).toContain("3 runs · 12 steps");
    expect(root.textContent).toContain("87.5% of tokens priced");
    expect(root.textContent).toContain("Cost class");
    expect(root.textContent).toContain("otel-event, proxy");
    expect(root.textContent).toContain("legacy run uses its immutable capture-date bucket");
    expect(root.textContent).toContain("never calls a model or reads captured payload text");
    expect(root.textContent).toContain("Hypothetical experiment cells never masquerade as runs");

    root.querySelector<HTMLButtonElement>('[data-scenario-model="anthropic/claude-haiku-4-5"]')?.click();
    expect(root.querySelector<HTMLInputElement>("#scenario-models")?.value).toBe("anthropic/claude-haiku-4-5");
    root.querySelector<HTMLButtonElement>("[data-scenario-use-cache]")?.click();
    expect(root.querySelector<HTMLInputElement>("#scenario-decache")?.checked).toBe(true);
  });

  it("posts a canonical scoped grid with quality constraints and renders deterministic honest results", async () => {
    const runExperiment = vi.fn(async (_request: ExperimentRequest) => EXPERIMENT_RESULT);
    const root = document.createElement("div");
    await renderOptimize(
      root,
      scenarioClient({ runExperiment }),
      parseHash("#/optimize?view=scenarios"),
      context()
    );

    const models = root.querySelector<HTMLInputElement>("#scenario-models")!;
    models.value = "z-model, a-model, z-model, *as-captured*";
    const snapshots = root.querySelector<HTMLInputElement>("#scenario-snapshots")!;
    snapshots.value = "2026-07-01, 2026-06-01, 2026-07-01";
    root.querySelector<HTMLInputElement>("#scenario-decache")!.checked = true;
    root.querySelector<HTMLInputElement>("#scenario-quality-min")!.value = "80";
    root.querySelector<HTMLInputElement>("#scenario-quality-max")!.value = "95";
    root.querySelector<HTMLButtonElement>("[data-scenario-run]")!.click();

    await vi.waitFor(() => expect(runExperiment).toHaveBeenCalledOnce());
    expect(runExperiment.mock.calls[0][0]).toEqual({
      cohort: SELECTION,
      experiment: {
        axes: [
          { kind: "model", values: ["*as-captured*", "a-model", "z-model"] },
          {
            kind: "pricing_snapshot",
            values: ["*as-captured*", "2026-06-01", "2026-07-01"],
          },
          { kind: "cache_strategy", values: ["*as-captured*", "decache"] },
        ],
      },
      quality_constraint: { min: 80, max: 95 },
    });
    expect(JSON.stringify(runExperiment.mock.calls[0][0])).not.toContain("payload");

    await vi.waitFor(() => expect(root.querySelectorAll("[data-scenario-cell]")).toHaveLength(2));
    expect(root.textContent).toContain("As captured");
    expect(root.textContent).toContain("Lowest estimate");
    expect(root.textContent).toContain("Estimated reduction");
    expect(root.textContent).toContain("$6.00");
    expect(root.textContent).toContain("Approximate tokenizer reprice");
    expect(root.textContent).toContain("Counts-preserving reprice");
    expect(root.textContent).toContain("Unpriced targets are omitted, never shown as $0");
    expect(root.textContent).toContain("pricing 2026-07-01");
  });

  it("hands only selected captured runs to Compare and preserves them in shared analysis state", async () => {
    const ctx = context();
    const root = document.createElement("div");
    await renderOptimize(
      root,
      scenarioClient(),
      parseHash("#/optimize?view=scenarios"),
      ctx
    );

    const compare = root.querySelector<HTMLAnchorElement>("[data-scenario-compare]")!;
    expect(compare.getAttribute("aria-disabled")).toBe("true");
    for (const id of ["run-b", "run-a"]) {
      const checkbox = root.querySelector<HTMLInputElement>(`[data-frontier-run="${id}"]`)!;
      checkbox.checked = true;
      checkbox.dispatchEvent(new Event("change"));
    }
    expect(compare.hasAttribute("aria-disabled")).toBe(false);
    expect(compare.getAttribute("href")).toContain("#/investigate/compare?");
    expect(compare.getAttribute("href")).toContain("runs=run-a%2Crun-b");
    compare.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    expect(ctx.analysis.get().workspace).toBe("investigate");
    expect(ctx.analysis.get().comparison.map((item) => item.id)).toEqual(["run-a", "run-b"]);
  });

  it("rejects malformed pricing dates before calling the experiment API", async () => {
    const runExperiment = vi.fn(async () => EXPERIMENT_RESULT);
    const root = document.createElement("div");
    await renderOptimize(
      root,
      scenarioClient({ runExperiment }),
      parseHash("#/optimize?view=scenarios"),
      context()
    );
    root.querySelector<HTMLInputElement>("#scenario-snapshots")!.value = "July 1";
    root.querySelector<HTMLButtonElement>("[data-scenario-run]")!.click();
    expect(runExperiment).not.toHaveBeenCalled();
    expect(root.querySelector("[role=status]")?.textContent).toContain("must use YYYY-MM-DD");
  });
});
