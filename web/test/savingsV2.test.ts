// Additive SavingsLedgerV2 over the client: v1 consumers keep reading the
// retained fields; v2 consumers get the OpportunityV2 evidence rows + capped/applied/observed totals.

import { describe, it, expect, vi, afterEach } from "vitest";
import { createHttpClient } from "../src/httpClient.js";
import type { SavingsLedger } from "../src/client.js";

afterEach(() => vi.unstubAllGlobals());

const V2: SavingsLedger = {
  opportunities: [
    { kind: "loop", label: "search", recoverable_micros: 100, confidence: "measured", fix_text: "dedupe", effort: "S" },
  ],
  total_recoverable_micros: 100,
  total_spend_micros: 1000,
  savings_index: 10,
  pricing_version: "v1",
  estimated: true,
  opportunities_v2: [
    {
      opportunity_key: "loop:0123456789abcdef",
      kind: "loop",
      label: "search",
      recoverable_micros: 100,
      confidence: "measured",
      fix_text: "dedupe",
      effort: "S",
      affected_run_count: 2,
      affected_step_count: 7,
      affected_run_ids: ["r1", "r2"],
      affected_steps: [{ run_id: "r1", step_ordinal: 3 }],
      evidence_truncated: false,
      evidence_method: "steps whose tool/agent label == `search`",
      cohort_snapshot: {
        timezone: "UTC",
        entity: "run",
        filters: [{ op: "run_ids", ids: ["r1", "r2"] }],
        pricing: { mode: "effective_dated" },
        metric: "spend_micros",
        normalization: "absolute",
      },
      assumptions: ["measured: this spend was already incurred on wasted work"],
    },
  ],
  capped_potential_micros: 100,
  applied_micros: 0,
  observed_micros: 0,
};

describe("SavingsLedgerV2 over HTTP", () => {
  it("carries v1 fields AND the additive v2 evidence rows", async () => {
    vi.stubGlobal("fetch", async () => new Response(JSON.stringify(V2)));
    const c = createHttpClient();
    const led = await c.savings();
    // v1 consumers keep working.
    expect(led.total_recoverable_micros).toBe(100);
    expect(led.opportunities[0].kind).toBe("loop");
    // v2 additions are present + honest.
    expect(led.capped_potential_micros).toBe(led.total_recoverable_micros);
    expect(led.applied_micros).toBe(0);
    expect(led.observed_micros).toBe(0);
    const o = led.opportunities_v2![0];
    expect(o.opportunity_key.startsWith("loop:")).toBe(true);
    expect(o.affected_step_count).toBe(7); // full count preserved even if the inline list is capped
    expect(o.cohort_snapshot.filters[0]).toEqual({ op: "run_ids", ids: ["r1", "r2"] });
    expect(o.quality_risk).toBeUndefined(); // measured waste has no model-swap quality risk
  });
});
