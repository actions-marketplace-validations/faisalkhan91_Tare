import { describe, it, expect, vi, afterEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createHttpClient } from "../src/httpClient.js";

function golden(name: string): string {
  return readFileSync(resolve(process.cwd(), "../tare-core/tests/golden", name), "utf8");
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("http TareClient (loopback /__tare/* read API)", () => {
  it("maps each method to its endpoint and parses JSON", async () => {
    const calls: string[] = [];
    vi.stubGlobal("fetch", async (url: string) => {
      calls.push(url);
      if (url.includes("/__tare/runs")) return new Response('["r1","r2"]');
      if (url.includes("/__tare/trend")) return new Response(golden("trend.json"));
      if (url.includes("/__tare/flamegraph")) return new Response(golden("flamegraph_bloated.json"));
      return new Response("{}");
    });
    const c = createHttpClient("http://127.0.0.1:8788");

    expect(await c.listRuns()).toEqual(["r1", "r2"]);
    const trend = await c.trend({ by: "provider", from: "2026-06-20" });
    expect(trend.dimension).toBe("by_provider");
    await c.flamegraph("run a/b");

    expect(calls[0]).toBe("http://127.0.0.1:8788/__tare/runs");
    expect(calls[1]).toContain("/__tare/trend?by=provider&from=2026-06-20");
    // run id is URL-encoded.
    expect(calls[2]).toContain("/__tare/flamegraph?run=run%20a%2Fb");
  });

  it("throws a visible error on a non-2xx response", async () => {
    vi.stubGlobal("fetch", async () => new Response("nope", { status: 500 }));
    const c = createHttpClient();
    await expect(c.report()).rejects.toThrow(/HTTP 500/);
  });

  it("encodes an optional burn-rate range without changing the legacy default request", async () => {
    const calls: string[] = [];
    vi.stubGlobal("fetch", async (url: string) => {
      calls.push(url);
      return new Response("{}");
    });
    const c = createHttpClient("http://127.0.0.1:8788");
    await c.burnrate("year");
    await c.burnrate();
    expect(calls).toEqual([
      "http://127.0.0.1:8788/__tare/burnrate?range=year",
      "http://127.0.0.1:8788/__tare/burnrate",
    ]);
  });
});
