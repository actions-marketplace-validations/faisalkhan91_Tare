import { describe, expect, it, vi } from "vitest";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";
import { cohortHash } from "../src/analysis/serialize.js";
import type {
  CohortSpec,
  SavingsAction,
  SavingsActionRequest,
  SavingsVerifyResult,
} from "../src/analysis/types.js";
import type { OpportunityV2, SavingsLedger } from "../src/client.js";
import { parseHash } from "../src/ui/store.js";
import { renderOptimize } from "../src/workspaces/optimize.js";
import { fakeClient } from "./fakeClient.js";

const SCOPE: CohortSpec = {
  from: "2026-07-01",
  to: "2026-07-07",
  timezone: "America/Los_Angeles",
  entity: "run",
  filters: [],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
  outcome_denominator: null,
};

function cohort(model: string): CohortSpec {
  return { ...SCOPE, filters: [{ op: "eq", dimension: "model", value: model }] };
}

function opportunity(
  key: string,
  model: string,
  recoverable_micros: number
): OpportunityV2 {
  return {
    opportunity_key: key,
    kind: "rightsizing",
    label: `${model} right-size`,
    recoverable_micros,
    confidence: "projected",
    fix_text: `model = "${model}-mini"`,
    effort: "S",
    affected_run_count: 2,
    affected_step_count: 3,
    affected_run_ids: [`run-${model}`],
    affected_steps: [{ run_id: `run-${model}`, step_ordinal: 1 }],
    evidence_truncated: false,
    evidence_method: "model rightsizing detector",
    cohort_snapshot: cohort(model),
    assumptions: ["Quality remains inside the stored guardrail."],
    quality_risk: "Validate output quality before broad rollout.",
  };
}

function action(o: OpportunityV2, status: "applied" | "dismissed"): SavingsAction {
  return {
    opportunity_key: o.opportunity_key,
    cohort_hash: cohortHash(o.cohort_snapshot),
    status,
    acted_at: "2026-07-08T12:00:00Z",
    cohort: o.cohort_snapshot,
    match: { kind: "workload_key", key: "checkout" },
    metric: o.cohort_snapshot.metric,
    normalization: o.cohort_snapshot.normalization,
    expected_point_micros: o.recoverable_micros,
    compatibility_warnings: [],
  };
}

function verification(
  status: SavingsVerifyResult["status"],
  observed_reduction_micros = 0
): SavingsVerifyResult {
  return {
    status,
    complete: status !== "verifying",
    selection_before_micros: 10_000_000,
    // No baseline is stored in these lifecycle fixtures, so the response's observed value must
    // equal selection_before - selection_after. Keep the fixture contract-consistent.
    selection_after_micros: 10_000_000 - observed_reduction_micros,
    observed_reduction_micros,
    matched_before: 2,
    matched_after: 2,
    unmatched_before: 0,
    unmatched_after: 0,
    compatibility_warnings: [],
  };
}

const OPEN = opportunity("rightsizing:open", "open", 1_000_000);
const VERIFYING = opportunity("rightsizing:verifying", "verifying", 2_000_000);
const OBSERVED = opportunity("rightsizing:observed", "observed", 3_000_000);
const NOT_OBSERVED = opportunity("rightsizing:not-observed", "not-observed", 4_000_000);
const DISMISSED = opportunity("rightsizing:dismissed", "dismissed", 5_000_000);
const OPPORTUNITIES = [OPEN, VERIFYING, OBSERVED, NOT_OBSERVED, DISMISSED];

const LEDGER: SavingsLedger = {
  opportunities: OPPORTUNITIES.map((o) => ({
    kind: o.kind,
    label: o.label,
    recoverable_micros: o.recoverable_micros,
    confidence: o.confidence,
    fix_text: o.fix_text,
    effort: o.effort,
  })),
  opportunities_v2: OPPORTUNITIES,
  total_recoverable_micros: 15_000_000,
  capped_potential_micros: 15_000_000,
  // Compatibility producers still emit placeholder zeros; the lifecycle queue must derive live
  // applied/observed summaries from actions + verification rather than erasing those records.
  applied_micros: 0,
  observed_micros: 0,
  total_spend_micros: 30_000_000,
  savings_index: 50,
  pricing_version: "2026-07-01",
  estimated: true,
};

const ACTIONS = [
  action(VERIFYING, "applied"),
  action(OBSERVED, "applied"),
  action(NOT_OBSERVED, "applied"),
  action(DISMISSED, "dismissed"),
];

function lifecycleClient() {
  return fakeClient({
    savings: async () => LEDGER,
    savingsActions: async () => ACTIONS,
    verifySavings: async ({ opportunity_key }) => {
      if (opportunity_key === VERIFYING.opportunity_key) return verification("verifying");
      if (opportunity_key === OBSERVED.opportunity_key) {
        return verification("observed_reduction", 1_250_000);
      }
      return verification("not_observed");
    },
  });
}

function context() {
  const analysis = createAnalysisStore(initialAnalysisState("optimize"));
  analysis.setScope(SCOPE);
  analysis.set({
    baseline: {
      kind: "prior_window",
      label: "Prior window · 12 runs",
      cohort: { ...SCOPE, from: "2026-06-24", to: "2026-06-30" },
      sampleCount: 12,
    },
    match: { kind: "workload_key", key: "checkout" },
  });
  return { analysis };
}

describe("Optimize lifecycle workspace", () => {
  it("derives lifecycle views and never conflates potential, applied exposure, or observed reduction", async () => {
    const root = document.createElement("div");
    const ctx = context();
    const client = lifecycleClient();

    await renderOptimize(
      root,
      client,
      parseHash("#/optimize?from=2026-07-01&to=2026-07-07&view=open"),
      ctx
    );

    const summaries = root.querySelector(".optimize-summary-section")?.textContent ?? "";
    expect(summaries).toContain("Capped potential");
    expect(summaries).toContain("$15.00");
    expect(summaries).toContain("Applied exposure");
    expect(summaries).toContain("$9.00");
    expect(summaries).toContain("Observed reduction");
    expect(summaries).toContain("$1.25");
    expect(summaries).toContain("Do not add or subtract these figures");

    expect(root.querySelectorAll(".optimize-lifecycle-row")).toHaveLength(1);
    expect(root.querySelector(".optimize-lifecycle-row")?.textContent).toContain(OPEN.label);

    const beam = root.querySelector(".beam-optimize")!;
    expect(beam.querySelector('.tare-beam-outline [data-beam-key="open"]')?.textContent).toContain("$1.00");
    expect(beam.querySelector('.tare-beam-outline [data-beam-key="verifying"]')?.textContent).toContain("$2.00");
    expect(beam.querySelector('.tare-beam-outline [data-beam-key="applied"]')?.textContent).toContain("$7.00");
    expect(beam.querySelector(".tare-beam-measured")?.textContent).toContain("$1.25");

    const expected: Array<[string, number, string]> = [
      ["applied", 3, "Verifying"],
      ["verifying", 1, VERIFYING.label],
      ["observed-reduction", 1, OBSERVED.label],
      ["not-observed", 1, NOT_OBSERVED.label],
      ["dismissed", 1, DISMISSED.label],
    ];
    for (const [view, count, text] of expected) {
      await renderOptimize(
        root,
        client,
        parseHash(`#/optimize?from=2026-07-01&to=2026-07-07&view=${view}`),
        ctx
      );
      expect(root.querySelectorAll(".optimize-lifecycle-row")).toHaveLength(count);
      expect(root.querySelector(".optimize-queue")?.textContent).toContain(text);
    }
  });

  it("preserves the route scope in filters and posts the opportunity's exact cohort snapshot", async () => {
    const accepted = vi.fn(async (_req: SavingsActionRequest) => {});
    const dismissed = vi.fn(async (_req: SavingsActionRequest) => {});
    const client = fakeClient({
      savings: async () => ({ ...LEDGER, opportunities_v2: [OPEN], opportunities: [LEDGER.opportunities[0]] }),
      savingsActions: async () => [],
      acceptSavings: accepted,
      dismissSavings: dismissed,
    });
    const ctx = context();
    const root = document.createElement("div");
    const route = parseHash(
      "#/optimize?f=opaque-selection&from=2026-07-01&to=2026-07-07&view=open"
    );
    await renderOptimize(root, client, route, ctx);

    const verifyingHref = root
      .querySelector<HTMLElement>('[data-lifecycle-view="verifying"]')
      ?.getAttribute("href") ?? "";
    expect(verifyingHref).toContain("f=opaque-selection");
    expect(verifyingHref).toContain("from=2026-07-01");
    expect(verifyingHref).toContain("view=verifying");

    root.querySelector<HTMLButtonElement>('[data-action="apply"]')?.click();
    await vi.waitFor(() => expect(accepted).toHaveBeenCalledOnce());
    expect(accepted.mock.calls[0][0]).toEqual({
      opportunity_key: OPEN.opportunity_key,
      cohort: OPEN.cohort_snapshot,
      baseline: {
        kind: "prior_window",
        label: "Prior window · 12 runs",
        cohort: { ...SCOPE, from: "2026-06-24", to: "2026-06-30" },
        sample_count: 12,
      },
      match: { kind: "workload_key", key: "checkout" },
      metric: "spend_micros",
      normalization: "absolute",
      expected_point_micros: OPEN.recoverable_micros,
    });

    await vi.waitFor(() =>
      expect(root.querySelector('[data-action="apply"]')?.getAttribute("aria-busy")).toBeNull()
    );
    const dismissButton = root.querySelector<HTMLButtonElement>('[data-action="dismiss"]');
    expect(dismissButton).not.toBeNull();
    expect(dismissButton?.disabled).toBe(false);
    dismissButton!.click();
    await vi.waitFor(() => expect(dismissed).toHaveBeenCalledOnce());
    expect(dismissed.mock.calls[0][0].cohort).toEqual(OPEN.cohort_snapshot);
  });

  it("keeps legacy opportunities visible without inventing a stable action identity", async () => {
    const root = document.createElement("div");
    await renderOptimize(
      root,
      fakeClient({
        savings: async () => ({
          ...LEDGER,
          opportunities_v2: undefined,
          opportunities: [LEDGER.opportunities[0]],
          applied_micros: undefined,
          observed_micros: undefined,
        }),
      }),
      parseHash("#/optimize"),
      context()
    );
    expect(root.querySelector(".optimize-lifecycle-row")?.textContent).toContain(OPEN.label);
    expect(root.textContent).toContain("Lifecycle actions need v2 exact-cohort evidence");
    expect(root.querySelector('[data-action="apply"]')).toBeNull();
  });

  it("restores only the dismissed opportunity+cohort identity", async () => {
    const removed = vi.fn(async () => {});
    const dismissedAction = action(DISMISSED, "dismissed");
    const root = document.createElement("div");
    const client = fakeClient({
      savings: async () => ({
        ...LEDGER,
        opportunities: [LEDGER.opportunities[4]],
        opportunities_v2: [DISMISSED],
      }),
      savingsActions: async () => [dismissedAction],
      unacceptSavings: removed,
    });
    await renderOptimize(
      root,
      client,
      parseHash("#/optimize?view=dismissed"),
      context()
    );
    root.querySelector<HTMLButtonElement>('[data-action="unaccept"]')?.click();
    await vi.waitFor(() => expect(removed).toHaveBeenCalledOnce());
    expect(removed).toHaveBeenCalledWith({
      opportunity_key: DISMISSED.opportunity_key,
      cohort_hash: dismissedAction.cohort_hash,
    });
  });
});
