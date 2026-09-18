import { describe, expect, it, vi } from "vitest";
import { cohortHash } from "../src/analysis/serialize.js";
import type {
  CohortSpec,
  SavingsAction,
  SavingsVerifyResult,
} from "../src/analysis/types.js";
import type { OpportunityV2, SavingsLedger } from "../src/client.js";
import { parseHash } from "../src/ui/store.js";
import { renderOptimize } from "../src/workspaces/optimize.js";
import { verificationWindows } from "../src/workspaces/optimizeVerification.js";
import { fakeClient } from "./fakeClient.js";

const COHORT: CohortSpec = {
  from: null,
  to: null,
  timezone: "America/Los_Angeles",
  entity: "run",
  filters: [{ op: "eq", dimension: "workload_key", value: "checkout" }],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "per_outcome",
  outcome_denominator: { kind: "work_unit", name: "pull request" },
};

const OPPORTUNITY: OpportunityV2 = {
  opportunity_key: "rightsizing:verification",
  kind: "rightsizing",
  label: "Right-size checkout workload",
  recoverable_micros: 3_000_000,
  confidence: "projected",
  fix_text: 'model = "smaller-model"',
  effort: "S",
  affected_run_count: 4,
  affected_step_count: 8,
  affected_run_ids: ["run-a", "run-b", "run-c", "run-d"],
  affected_steps: [],
  evidence_truncated: false,
  evidence_method: "captured workload model mix",
  cohort_snapshot: COHORT,
  assumptions: ["Quality remains within the stored threshold."],
  quality_risk: "Validate representative outputs.",
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
  total_recoverable_micros: 3_000_000,
  capped_potential_micros: 3_000_000,
  total_spend_micros: 10_000_000,
  savings_index: 30,
  pricing_version: "2026-07-01",
  estimated: true,
};

function applied(overrides: Partial<SavingsAction> = {}): SavingsAction {
  return {
    opportunity_key: OPPORTUNITY.opportunity_key,
    cohort_hash: cohortHash(COHORT),
    status: "applied",
    // 02:00Z is still the prior local day in Los Angeles. The verifier excludes 2026-07-07.
    acted_at: "2026-07-08T02:00:00Z",
    cohort: COHORT,
    baseline: {
      kind: "prior_window",
      label: "Prior seven-day comparison",
      cohort: { ...COHORT, from: "2026-06-23", to: "2026-06-29" },
      sample_count: 12,
    },
    match: { kind: "workload_key", key: "checkout" },
    metric: "spend_micros",
    normalization: "per_outcome",
    outcome_denominator: { kind: "work_unit", name: "pull request" },
    expected_point_micros: 3_000_000,
    quality_guardrail: 85,
    compatibility_warnings: [],
    ...overrides,
  };
}

function result(overrides: Partial<SavingsVerifyResult> = {}): SavingsVerifyResult {
  return {
    status: "observed_reduction",
    complete: true,
    selection_before_micros: 10_000_000,
    selection_after_micros: 8_000_000,
    baseline_before_micros: 4_000_000,
    baseline_after_micros: 5_000_000,
    observed_reduction_micros: 3_000_000,
    matched_before: 4,
    matched_after: 4,
    unmatched_before: 2,
    unmatched_after: 1,
    baseline_matched_before: 4,
    baseline_matched_after: 4,
    baseline_unmatched_before: 3,
    baseline_unmatched_after: 2,
    compatibility_warnings: ["One captured source changed pricing edition."],
    ...overrides,
  };
}

async function renderVerification(
  action: SavingsAction,
  verification: SavingsVerifyResult,
  view: string
): Promise<{ root: HTMLElement; verifySavings: ReturnType<typeof vi.fn> }> {
  const verifySavings = vi.fn(async () => verification);
  const root = document.createElement("div");
  await renderOptimize(
    root,
    fakeClient({
      savings: async () => LEDGER,
      savingsActions: async () => [action],
      verifySavings,
    }),
    parseHash(`#/optimize?view=${view}`)
  );
  return { root, verifySavings };
}

describe("Optimize applied-change verification", () => {
  it("shows cohort-local windows, all four matched sets, adjusted formula, and guardrail limits", async () => {
    const action = applied();
    const { root, verifySavings } = await renderVerification(
      action,
      result(),
      "observed-reduction"
    );
    expect(verifySavings).toHaveBeenCalledWith({
      opportunity_key: action.opportunity_key,
      cohort_hash: action.cohort_hash,
    });

    const panel = root.querySelector(".optimize-verification")!;
    const text = panel.textContent ?? "";
    expect(text).toContain("Applied-change verification");
    expect(text).toContain("Complete window");
    expect(text).toContain("Observed reduction · $3.00 in the stored matched cohort");
    expect(text).toContain("Intervention day2026-07-07 · excluded · America/Los_Angeles");
    expect(text).toContain("Selection · before2026-06-30–2026-07-06$10.00");
    expect(text).toContain("Selection · after2026-07-08–2026-07-14$8.00");
    expect(text).toContain("4 matched · 2 excluded");
    expect(text).toContain("4 matched · 1 excluded");
    expect(text).toContain("Baseline · Prior seven-day comparison");
    expect(text).toContain("Baseline · before2026-06-30–2026-07-06$4.00");
    expect(text).toContain("Baseline · after2026-07-08–2026-07-14$5.00");
    expect(text).toContain("4 matched · 3 excluded");
    expect(text).toContain("4 matched · 2 excluded");
    expect(text).toContain("Adjusted observed reduction formula");
    expect(text).toContain("($10.00 − $8.00) + ($5.00 − $4.00) = +$3.00");
    expect(text).toContain("Workload key · checkout");
    expect(text).toContain("Work unit · pull request");
    expect(text).toContain("85 stored threshold");
    expect(text).toContain("pass/fail is unavailable");
    expect(text).toContain("One captured source changed pricing edition.");
    expect(text.toLowerCase()).not.toContain("realized savings");
  });

  it("keeps aggregate-only positive change labeled as an association", async () => {
    const warning = "aggregate-only action — observed association, not a causal saving; before/after units are not matched";
    const action = applied({
      baseline: undefined,
      match: { kind: "aggregate_only" },
      outcome_denominator: undefined,
      quality_guardrail: undefined,
      compatibility_warnings: [warning],
    });
    const verification = result({
      selection_before_micros: 9_000_000,
      selection_after_micros: 6_000_000,
      baseline_before_micros: undefined,
      baseline_after_micros: undefined,
      observed_reduction_micros: 3_000_000,
      matched_before: 0,
      matched_after: 0,
      unmatched_before: 5,
      unmatched_after: 4,
      baseline_matched_before: undefined,
      baseline_matched_after: undefined,
      baseline_unmatched_before: undefined,
      baseline_unmatched_after: undefined,
      compatibility_warnings: [warning],
    });
    const { root } = await renderVerification(action, verification, "observed-reduction");
    expect(root.querySelector(".optimize-state")?.textContent).toContain("Observed association");
    const panel = root.querySelector(".optimize-verification")!;
    const text = panel.textContent ?? "";
    expect(text).toContain("Observed association · $3.00 lower under aggregate-only");
    expect(text).toContain("this is not a causal saving");
    expect(text).toContain("0 like-for-like matched · 5 unmatched; spend remains full-cohort aggregate");
    expect(text).toContain("0 like-for-like matched · 4 unmatched; spend remains full-cohort aggregate");
    expect(text).toContain("No baseline was stored");
    expect(text).toContain("Unadjusted observed change formula");
    expect(text).toContain("no causal outcome-denominator claim");
    expect(text).toContain("Not stored; no quality pass/fail claim");
    expect(
      [...panel.querySelectorAll(".optimize-row-warning")]
        .filter((node) => node.textContent === warning)
    ).toHaveLength(1);
  });

  it("forces an incomplete response to remain Verifying and excludes it from observed totals", async () => {
    const verification = result({
      // A bad/stale transport status must not upgrade an incomplete window to an outcome.
      status: "observed_reduction",
      complete: false,
    });
    const { root } = await renderVerification(applied(), verification, "verifying");
    expect(root.querySelector(".optimize-lifecycle-row")?.getAttribute("data-lifecycle-state")).toBe("verifying");
    expect(root.querySelector(".optimize-state")?.textContent).toContain("Verifying");
    const panel = root.querySelector(".optimize-verification")!;
    expect(panel.textContent).toContain("Incomplete window");
    expect(panel.textContent).toContain("No observed outcome is claimed yet");
    expect(panel.textContent).toContain("response said Observed reduction");
    expect(panel.textContent).toContain("require Verifying");
    expect(root.querySelector(".optimize-summary.observed")?.textContent).toContain("$0.00");
    expect(root.querySelector(".tare-beam-measured")?.textContent).toContain("$0.00");
  });

  it("shows a complete non-positive result as Not observed without claiming no effect", async () => {
    const action = applied({ baseline: undefined, quality_guardrail: undefined });
    const verification = result({
      status: "not_observed",
      complete: true,
      selection_before_micros: 8_000_000,
      selection_after_micros: 9_000_000,
      baseline_before_micros: undefined,
      baseline_after_micros: undefined,
      observed_reduction_micros: -1_000_000,
      baseline_matched_before: undefined,
      baseline_matched_after: undefined,
      baseline_unmatched_before: undefined,
      baseline_unmatched_after: undefined,
    });
    const { root } = await renderVerification(action, verification, "not-observed");
    expect(root.querySelector(".optimize-state")?.textContent).toContain("Not observed");
    const text = root.querySelector(".optimize-verification")?.textContent ?? "";
    expect(text).toContain("No positive reduction was measured (-$1.00)");
    expect(text).toContain("This does not prove the change had no effect");
  });

  it("suppresses an arithmetically inconsistent outcome from lifecycle totals", async () => {
    const verification = result({
      // Window values compute +$3, but this stale/corrupt response reports +$2.
      observed_reduction_micros: 2_000_000,
    });
    const { root } = await renderVerification(applied(), verification, "applied");
    expect(root.querySelector(".optimize-lifecycle-row")?.getAttribute("data-lifecycle-state")).toBe("applied");
    expect(root.querySelector(".optimize-state")?.textContent).toContain("Applied");
    const text = root.querySelector(".optimize-verification")?.textContent ?? "";
    expect(text).toContain("No observed outcome is claimed");
    expect(text).toContain("response reports +$2.00");
    expect(text).toContain("window values compute +$3.00");
    expect(root.querySelector(".optimize-summary.observed")?.textContent).toContain("$0.00");
    expect(root.querySelector(".tare-beam-measured")?.textContent).toContain("$0.00");
  });

  it("derives the intervention date in the stored IANA zone across DST", () => {
    expect(
      verificationWindows(applied({ acted_at: "2026-03-08T09:30:00Z" }))
    ).toEqual({
      intervention: "2026-03-08",
      beforeFrom: "2026-03-01",
      beforeTo: "2026-03-07",
      afterFrom: "2026-03-09",
      afterTo: "2026-03-15",
      usedDatePrefixFallback: false,
    });
  });
});
