import { describe, it, expect } from "vitest";
import { cachingClient } from "../src/ui/cachingClient.js";
import type { TareClient } from "../src/client.js";

// A minimal fake that counts transport hits per method, so we can assert what actually reached it.
function fakeClient() {
  const calls: Record<string, number> = {};
  let actionState = "open";
  const bump = (m: string) => (calls[m] = (calls[m] ?? 0) + 1);
  const client = {
    report: async () => (bump("report"), { total: 1 }),
    rollup: async (by: string) => (bump("rollup"), { by }),
    today: async () => (bump("today"), { spend: 1 }), // realtime → never cached
    saveQuality: async () => void bump("saveQuality"), // mutation → clears cache
    listInvestigations: async () => (bump("listInvestigations"), []),
    saveInvestigation: async () => void bump("saveInvestigation"),
    savingsActions: async () => (bump("savingsActions"), [{ status: actionState }]),
    acceptSavings: async () => {
      bump("acceptSavings");
      actionState = "applied";
    },
    dismissSavings: async () => {
      bump("dismissSavings");
      actionState = "dismissed";
    },
    unacceptSavings: async () => {
      bump("unacceptSavings");
      actionState = "open";
    },
    coverage: async () => (bump("coverage"), { status: "green" }),
    verifySavings: async () => (bump("verifySavings"), { status: "verifying" }),
    boom: async () => {
      bump("boom");
      throw new Error("nope");
    },
  } as unknown as TareClient;
  return { client, calls };
}

// Controllable clock.
function clock(start = 1000) {
  let t = start;
  return { now: () => t, advance: (ms: number) => (t += ms) };
}

describe("cachingClient", () => {
  it("serves a fresh hit without touching the transport", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await c.report();
    await c.report();
    expect(calls.report).toBe(1);
  });

  it("dedupes concurrent identical reads into one round-trip", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await Promise.all([c.rollup("total"), c.rollup("total"), c.rollup("total")]);
    expect(calls.rollup).toBe(1);
  });

  it("keys on args — different args are separate entries", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await c.rollup("total");
    await c.rollup("model");
    expect(calls.rollup).toBe(2);
  });

  it("refetches after the freshness window expires", async () => {
    const { client, calls } = fakeClient();
    const clk = clock();
    const c = cachingClient(client, { freshMs: 10_000, now: clk.now }) as any;
    await c.report();
    clk.advance(10_001);
    await c.report();
    expect(calls.report).toBe(2);
  });

  it("never caches realtime reads", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await c.today();
    await c.today();
    expect(calls.today).toBe(2);
  });

  it("clears the cache on a mutation so the next read reflects the write", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await c.report(); // cached
    await c.saveQuality();
    await c.report(); // must refetch
    expect(calls.report).toBe(2);
  });

  it("treats v2 investigation writes as mutations, not cacheable reads", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await c.listInvestigations();
    await c.listInvestigations();
    expect(calls.listInvestigations).toBe(1);
    await c.saveInvestigation({ id: "inv" });
    await c.listInvestigations();
    expect(calls.saveInvestigation).toBe(1);
    expect(calls.listInvestigations).toBe(2);
  });

  it("invalidates cached lifecycle reads after apply, dismiss, and restore", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    expect((await c.savingsActions())[0].status).toBe("open");
    await c.acceptSavings({});
    expect((await c.savingsActions())[0].status).toBe("applied");
    await c.dismissSavings({});
    expect((await c.savingsActions())[0].status).toBe("dismissed");
    await c.unacceptSavings({});
    expect((await c.savingsActions())[0].status).toBe("open");
    expect(calls.savingsActions).toBe(4);
  });

  it("never caches live capture health or cohort verification", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await c.coverage();
    await c.coverage();
    await c.verifySavings({ opportunity_key: "x", cohort_hash: "y" });
    await c.verifySavings({ opportunity_key: "x", cohort_hash: "y" });
    expect(calls.coverage).toBe(2);
    expect(calls.verifySavings).toBe(2);
  });

  it("does not cache failures — the next call retries", async () => {
    const { client, calls } = fakeClient();
    const c = cachingClient(client, { now: () => 1000 }) as any;
    await expect(c.boom()).rejects.toThrow("nope");
    await expect(c.boom()).rejects.toThrow("nope");
    expect(calls.boom).toBe(2);
  });

  it("passes synchronous capability predicates through UNWRAPPED", () => {
    // Regression guard: the default caching branch wraps returns in Promise.resolve(...). A sync
    // `can*` predicate must NOT become a (always-truthy) Promise, or `if (client.canX())` desktop-only
    // gates would fire in the browser. NO_CACHE returns them verbatim.
    const client = {
      canNotify: () => false,
      canControlProxy: () => false,
      canBackgroundOnClose: () => false,
    } as unknown as TareClient;
    const c = cachingClient(client, { now: () => 1000 }) as any;
    expect(c.canNotify()).toBe(false); // a real boolean, not a truthy Promise
    expect(c.canControlProxy()).toBe(false);
    expect(c.canBackgroundOnClose()).toBe(false);
  });
});
