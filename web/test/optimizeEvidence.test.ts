import { describe, expect, it, vi } from "vitest";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { cohortHash } from "../src/analysis/serialize.js";
import type {
  AnalysisProvenance,
  CohortSpec,
  SavingsAction,
} from "../src/analysis/types.js";
import type { OpportunityV2, SavingsLedger } from "../src/client.js";
import { parseHash } from "../src/ui/store.js";
import { renderOptimize } from "../src/workspaces/optimize.js";
import { fakeClient } from "./fakeClient.js";

const COHORT: CohortSpec = {
  from: "2026-07-01",
  to: "2026-07-07",
  timezone: "America/Los_Angeles",
  entity: "step",
  filters: [{ op: "eq", dimension: "model", value: "evidence-model" }],
  pricing: { mode: "as_of", date: "2026-07-01" },
  metric: "spend_micros",
  normalization: "absolute",
  outcome_denominator: null,
};

const OPPORTUNITY: OpportunityV2 = {
  opportunity_key: "rightsizing:evidence",
  kind: "rightsizing",
  label: "Right-size evidence-model",
  recoverable_micros: 2_500_000,
  confidence: "projected",
  fix_text: 'model = "evidence-model-mini"',
  effort: "S",
  affected_run_count: 3,
  affected_step_count: 4,
  affected_run_ids: ["run-a", "run-b", "run-c"],
  affected_steps: [
    { run_id: "run-a", step_ordinal: 1 },
    { run_id: "run-a", step_ordinal: 2 },
    { run_id: "run-b", step_ordinal: 1 },
    { run_id: "run-c", step_ordinal: 7 },
  ],
  evidence_truncated: false,
  evidence_method: "stored model price and captured token counts",
  cohort_snapshot: COHORT,
  assumptions: ["The candidate model can serve this workload."],
  quality_risk: "Validate output quality on a representative sample.",
};

const LEDGER: SavingsLedger = {
  opportunities: [{
    kind: OPPORTUNITY.kind,
    label: OPPORTUNITY.label,
    recoverable_micros: OPPORTUNITY.recoverable_micros,
    confidence: OPPORTUNITY.confidence,
    fix_text: OPPORTUNITY.fix_text,
    effort: OPPORTUNITY.effort,
  }],
  opportunities_v2: [OPPORTUNITY],
  total_recoverable_micros: OPPORTUNITY.recoverable_micros,
  capped_potential_micros: OPPORTUNITY.recoverable_micros,
  applied_micros: 0,
  observed_micros: 0,
  total_spend_micros: 10_000_000,
  savings_index: 25,
  pricing_version: "2026-07-01",
  estimated: true,
};

const PROVENANCE: AnalysisProvenance = {
  refreshed_at: "2026-07-16T12:00:00Z",
  scope: COHORT,
  capture_sources: ["proxy", "otel-span"],
  coverage_status: "partial",
  priced_token_share_pct: 80,
  component_fidelity: "cost_class",
  pricing_edition: {
    version: "2026-07-01",
    effective_date: "2026-07-01",
    mode: "as_of",
  },
  allocation_method: "provider_counts",
  value_class: "derived",
  assumptions: ["Provider counters were allocated to captured steps."],
};

function context() {
  const analysis = createAnalysisStore(initialAnalysisState("optimize"));
  analysis.setScope(COHORT);
  analysis.set({
    baseline: {
      kind: "prior_window",
      label: "Prior seven days",
      cohort: { ...COHORT, from: "2026-06-24", to: "2026-06-30" },
      sampleCount: 9,
    },
    match: { kind: "aggregate_only" },
  });
  return { analysis };
}

describe("Optimize opportunity evidence inspector", () => {
  it("renders every required claim from v2, the exact-cohort resolve, and provenance", async () => {
    const resolveCohort = vi.fn(async (cohort: CohortSpec) => ({
      data: {
        run_ids: ["run-a", "run-b", "run-c"],
        step_refs: OPPORTUNITY.affected_steps,
        run_count: 3,
        step_count: 4,
        total_micros: 10_000_000,
        entity_rows: [],
      },
      provenance: { ...PROVENANCE, scope: cohort },
    }));
    const root = document.createElement("div");
    await renderOptimize(
      root,
      fakeClient({
        savings: async () => LEDGER,
        savingsActions: async () => [],
        resolveCohort,
      }),
      parseHash("#/optimize?view=open"),
      context()
    );

    expect(resolveCohort).not.toHaveBeenCalled();
    const inspect = root.querySelector<HTMLButtonElement>('[data-action="inspect-evidence"]')!;
    inspect.click();
    await vi.waitFor(() => expect(resolveCohort).toHaveBeenCalledWith(COHORT));
    await vi.waitFor(() =>
      expect(root.querySelector(".optimize-evidence-body")).not.toBeNull()
    );

    const inspector = root.querySelector<HTMLElement>(".optimize-evidence-inspector")!;
    const text = inspector.textContent ?? "";
    expect(inspect.getAttribute("aria-expanded")).toBe("true");
    expect(text).toContain("Affected current spend$10.00");
    expect(text).toContain("Point $2.50 · conservative/high not supplied by detector");
    expect(text).toContain("Percentage of scoped spend25% of resolved exact-cohort spend");
    expect(text).toContain("Recurrence window2026-07-01–2026-07-07");
    expect(text).toContain("3 affected runs · 4 affected steps");
    expect(text).toContain("Run references · 3 of 3 inline");
    expect(text).toContain("run-c");
    expect(text).toContain("run-c#7");
    expect(text).toContain(OPPORTUNITY.evidence_method);
    expect(text).toContain("Detector confidenceProjected");
    expect(text).toContain("EffortS");
    expect(text).toContain(OPPORTUNITY.assumptions[0]);
    expect(text).toContain(OPPORTUNITY.quality_risk);
    expect(text).toContain("TimezoneAmerica/Los_Angeles");
    expect(text).toContain("Model = evidence-model");
    expect(text).toContain("Action snapshot to persist");
    expect(text).toContain(OPPORTUNITY.opportunity_key);
    expect(text).toContain("Prior seven days · 9 samples");
    expect(text).toContain("CoveragePartial");
    expect(text).toContain("80% priced · 20% usage-only and excluded from dollars");
    expect(text).toContain("proxy, otel-span");
    expect(text).toContain(PROVENANCE.assumptions[0]);
    expect(root.querySelector('[data-action="copy"]')).not.toBeNull();
    expect(root.querySelector('[data-action="apply"]')).not.toBeNull();
    expect(root.querySelector('[data-action="dismiss"]')).not.toBeNull();

    inspect.click();
    expect(inspect.getAttribute("aria-expanded")).toBe("false");
    expect(inspector.hasAttribute("hidden")).toBe(true);
    inspect.click();
    expect(resolveCohort).toHaveBeenCalledOnce();
  });

  it("attributes a saved conservative/point/high range to the persisted action snapshot", async () => {
    const saved: SavingsAction = {
      opportunity_key: OPPORTUNITY.opportunity_key,
      cohort_hash: cohortHash(COHORT),
      status: "applied",
      acted_at: "2026-07-15T19:00:00Z",
      cohort: COHORT,
      match: { kind: "aggregate_only" },
      metric: "spend_micros",
      normalization: "absolute",
      expected_low_micros: 1_000_000,
      expected_point_micros: 2_000_000,
      expected_high_micros: 3_000_000,
      quality_guardrail: 90,
      compatibility_warnings: [],
    };
    const root = document.createElement("div");
    await renderOptimize(
      root,
      fakeClient({
        savings: async () => LEDGER,
        savingsActions: async () => [saved],
        resolveCohort: async () => ({
          data: { run_ids: [], run_count: 0, step_count: 0, total_micros: 10_000_000, entity_rows: [] },
          provenance: PROVENANCE,
        }),
      }),
      parseHash("#/optimize?view=applied"),
      context()
    );
    root.querySelector<HTMLButtonElement>('[data-action="inspect-evidence"]')!.click();
    await vi.waitFor(() => expect(root.querySelector(".optimize-evidence-body")).not.toBeNull());
    const text = root.querySelector(".optimize-evidence-inspector")?.textContent ?? "";
    expect(text).toContain("Conservative $1.00 · point $2.00 · high $3.00 · saved action snapshot");
    expect(text).toContain("Stored action snapshot");
    expect(text).toContain("Applied at 2026-07-15T19:00:00Z");
    expect(text).toContain("Quality guardrail90");
  });

  it("drills truncated inline evidence through the exact cohort", async () => {
    const truncated: OpportunityV2 = {
      ...OPPORTUNITY,
      opportunity_key: "rightsizing:truncated",
      affected_run_count: 701,
      affected_step_count: 913,
      evidence_truncated: true,
    };
    const ctx = context();
    const root = document.createElement("div");
    await renderOptimize(
      root,
      fakeClient({
        savings: async () => ({ ...LEDGER, opportunities_v2: [truncated] }),
        savingsActions: async () => [],
        resolveCohort: async () => ({
          data: { run_ids: [], run_count: 701, step_count: 913, total_micros: 10_000_000, entity_rows: [] },
          provenance: PROVENANCE,
        }),
      }),
      parseHash("#/optimize?view=open"),
      ctx
    );
    root.querySelector<HTMLButtonElement>('[data-action="inspect-evidence"]')!.click();
    await vi.waitFor(() => expect(root.querySelector('[data-action="drill-evidence"]')).not.toBeNull());
    const inspector = root.querySelector(".optimize-evidence-inspector")!;
    expect(inspector.textContent).toContain("Inline evidence is truncated");
    expect(inspector.textContent).toContain("Counts remain complete");
    const drill = inspector.querySelector<HTMLAnchorElement>('[data-action="drill-evidence"]')!;
    expect(drill.textContent).toBe("Inspect complete exact cohort");
    expect(drill.getAttribute("href")).toContain("#/investigate?");
    expect(drill.getAttribute("href")).toContain("sel=");
    drill.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }));
    expect(ctx.analysis.get().selection).toEqual(COHORT);
  });

  it("keeps claims unavailable and actions usable when exact-cohort resolution fails", async () => {
    const root = document.createElement("div");
    await renderOptimize(
      root,
      fakeClient({
        savings: async () => LEDGER,
        savingsActions: async () => [],
        resolveCohort: async () => {
          throw new Error("offline");
        },
      }),
      parseHash("#/optimize?view=open"),
      context()
    );
    root.querySelector<HTMLButtonElement>('[data-action="inspect-evidence"]')!.click();
    await vi.waitFor(() =>
      expect(root.querySelector(".optimize-evidence-body")).not.toBeNull()
    );
    const text = root.querySelector(".optimize-evidence-inspector")?.textContent ?? "";
    expect(text).toContain("Affected current spendUnavailable");
    expect(text).toContain("Percentage of scoped spendUnavailable");
    expect(text).toContain("no completeness claim is made");
    expect(root.querySelector('[data-action="copy"]')).not.toBeNull();
    expect(root.querySelector('[data-action="apply"]')).not.toBeNull();
    expect(root.querySelector('[data-action="dismiss"]')).not.toBeNull();
  });
});
