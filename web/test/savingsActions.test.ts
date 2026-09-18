// Savings action lifecycle transport: apply/dismiss/unaccept + list.

import { describe, it, expect, vi, afterEach } from "vitest";
import { createHttpClient } from "../src/httpClient.js";
import { createTauriClient } from "../src/tauriClient.js";
import { fakeClient } from "./fakeClient.js";
import type {
  CohortSpec,
  SavingsAction,
  SavingsActionRequest,
  SavingsVerifyResult,
} from "../src/analysis/types.js";

const SPEC: CohortSpec = {
  timezone: "UTC",
  entity: "run",
  filters: [],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
};
const REQ: SavingsActionRequest = {
  opportunity_key: "loop:search",
  cohort: SPEC,
  match: { kind: "aggregate_only" },
  metric: "spend_micros",
  normalization: "absolute",
};

afterEach(() => vi.unstubAllGlobals());

describe("savings action transport", () => {
  it("HTTP posts accept/dismiss/unaccept to the right paths and GETs the list", async () => {
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
      calls.push([url, init?.body ? JSON.parse(init.body as string) : undefined]);
      if (url.includes("/savings/actions")) return new Response("[]");
      return new Response('{"ok":true}');
    });
    const c = createHttpClient("http://127.0.0.1:8788");
    await c.acceptSavings(REQ);
    await c.dismissSavings(REQ);
    await c.unacceptSavings({ opportunity_key: "loop:search", cohort_hash: "abc" });
    await c.savingsActions();
    expect(calls[0][0]).toBe("http://127.0.0.1:8788/__tare/savings/accept");
    expect(calls[0][1]).toEqual(REQ); // full request POSTed verbatim
    expect(calls[1][0]).toContain("/__tare/savings/dismiss");
    expect(calls[2][0]).toContain("/__tare/savings/unaccept");
    expect(calls[2][1]).toEqual({ opportunity_key: "loop:search", cohort_hash: "abc" });
    expect(calls[3][0]).toContain("/__tare/savings/actions");
  });

  it("propagates a 409 (incompatible transition / collision)", async () => {
    vi.stubGlobal("fetch", async () => new Response('{"error":"incompatible"}', { status: 409 }));
    const c = createHttpClient();
    await expect(c.dismissSavings(REQ)).rejects.toThrow(/HTTP 409/);
  });

  it("Tauri invokes the savings_* commands with a JSON body", async () => {
    const calls: Array<[string, unknown]> = [];
    const c = createTauriClient(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return cmd === "savings_actions" ? "[]" : undefined;
    });
    await c.acceptSavings(REQ);
    await c.savingsActions();
    expect(calls[0][0]).toBe("savings_accept");
    expect(calls[0][1]).toEqual({ body: JSON.stringify(REQ) });
    expect(calls[1][0]).toBe("savings_actions");
  });

  it("fake round-trips apply → list → unaccept, keyed per opportunity+cohort", async () => {
    const c = fakeClient();
    await c.acceptSavings(REQ);
    let acts: SavingsAction[] = await c.savingsActions();
    expect(acts.length).toBe(1);
    expect(acts[0].status).toBe("applied");
    // aggregate-only carries the confounding warning.
    expect(acts[0].compatibility_warnings.length).toBeGreaterThan(0);
    await c.unacceptSavings({ opportunity_key: REQ.opportunity_key, cohort_hash: acts[0].cohort_hash });
    acts = await c.savingsActions();
    expect(acts.length).toBe(0);
  });

  it("verifySavings posts to /savings/verify (HTTP) / invokes savings_verify (Tauri), equivalent", async () => {
    const RESULT: SavingsVerifyResult = {
      status: "verifying",
      complete: false,
      selection_before_micros: 100,
      selection_after_micros: 80,
      baseline_before_micros: 50,
      baseline_after_micros: 50,
      observed_reduction_micros: 20,
      matched_before: 1,
      matched_after: 1,
      unmatched_before: 0,
      unmatched_after: 0,
      baseline_matched_before: 1,
      baseline_matched_after: 1,
      baseline_unmatched_before: 2,
      baseline_unmatched_after: 3,
      compatibility_warnings: ["aggregate-only action"],
    };
    const id = { opportunity_key: "loop:search", cohort_hash: "abc", window_days: 7 };
    const calls: Array<[string, unknown]> = [];
    vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
      calls.push([url, init?.body ? JSON.parse(init.body as string) : undefined]);
      return new Response(JSON.stringify(RESULT));
    });
    const http = createHttpClient("http://127.0.0.1:8788");
    const viaHttp = await http.verifySavings(id);
    expect(viaHttp.status).toBe("verifying");
    expect(viaHttp.observed_reduction_micros).toBe(20);
    expect(viaHttp.baseline_unmatched_after).toBe(3);
    expect(calls[0][0]).toBe("http://127.0.0.1:8788/__tare/savings/verify");
    expect(calls[0][1]).toEqual(id);

    const tauri = createTauriClient(async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      return JSON.stringify(RESULT);
    });
    const viaTauri = await tauri.verifySavings(id);
    expect(viaTauri).toEqual(viaHttp); // equivalent over both transports
    expect(calls[1]).toEqual(["savings_verify", { body: JSON.stringify(id) }]);
  });
});
