// Page-by-page integration: mount the full shell against a populated client and visit every
// route, asserting each renders content and never an error pane. This is the "run every page"
// smoke in code (complements the live `tare serve` route sweep).

import { describe, it, expect, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { mountApp } from "../src/main.js";
import { fakeClient } from "./fakeClient.js";
import { setOnboarded } from "../src/ui/prefs.js";
import type { TareClient } from "../src/client.js";
import { SECTIONS, LENS_ROUTES } from "../src/shell/rail.js";

function golden(name: string): string {
  return readFileSync(resolve(process.cwd(), "../tare-core/tests/golden", name), "utf8");
}

function populated(): TareClient {
  return fakeClient({
    listRuns: async () => ["run-a", "run-b"],
    flamegraph: async () => JSON.parse(golden("flamegraph_bloated.json")),
    report: async () => JSON.parse(golden("report.json")),
    trend: async () => JSON.parse(golden("trend.json")),
    today: async () => ({ total_micros: 4_210_000, pricing_version: "x", effective_date: "d" }),
    runStatus: async (id: string) => ({ run_id: id, micros: 1_920_000, steps: 3, top_cause: "bloated-system-prompt" }),
    runStatuses: async () => [
      { run_id: "run-a", micros: 1_920_000, steps: 3, top_cause: "bloated-system-prompt" },
      { run_id: "run-b", micros: 1_920_000, steps: 3, top_cause: "bloated-system-prompt" },
    ],
    explain: async () => "Mostly an uncached system prompt.",
    rollup: async () => ({
      dimension: "step",
      rows: [{ label: "planner", runs: 1, steps: 3, tokens: 1000, micros: 500000, micros_per_call: 166666 }],
      total_micros: 500000,
      pricing_version: "x",
      estimated: true,
    }),
    advise: async () => [],
    whatif: async () => ({ baseline_micros: 1000, recommendations: [], estimated: true, approximate: true }),
    config: async () => ({ budget: { max_spend_usd: 10 }, privacy: {}, providers: {}, proxy: {} }),
    anomalies: async () => [],
  });
}

const ROUTES = [
  "#/live",
  "#/overview",
  "#/runs",
  "#/runs/run-a",
  "#/trends",
  "#/optimize",
  "#/diff",
  "#/connect",
  "#/settings",
];

describe("every page renders", () => {
  beforeEach(() => setOnboarded(true));

  for (const hash of ROUTES) {
    it(`renders ${hash} with no error pane`, async () => {
      location.hash = hash;
      const root = document.createElement("div");
      document.body.appendChild(root);
      await mountApp(root, populated());
      // let any async screen body settle (Optimize/Overview tabs, run detail fan-out)
      await new Promise((r) => setTimeout(r, 5));
      const main = root.querySelector(".main")!;
      expect(main).toBeTruthy();
      expect(main.textContent?.length).toBeGreaterThan(0);
      expect(main.querySelector(".error"), `${hash} showed an error pane`).toBeNull();
      root.remove();
      location.hash = "";
    });
  }

  it("redirects #/overview to Pulse with no Summary or Segments tabs", async () => {
    location.hash = "#/overview";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    await new Promise((r) => setTimeout(r, 5));
    // The Monitor screens cut over to Pulse: the legacy hash is rewritten and Pulse renders.
    expect(location.hash).toBe("#/pulse");
    expect(root.querySelector(".main .pulse")).toBeTruthy();
    // Summary and Segments tabs are absent because segmentation moved to Investigate facets.
    expect(root.querySelector(".tab-inline")).toBeNull();
    expect(root.querySelector(".tabs-inline")).toBeNull();
    root.remove();
    location.hash = "";
  });

  it("redirects #/live to Pulse focused on the Now feed", async () => {
    location.hash = "#/live";
    const root = document.createElement("div");
    document.body.appendChild(root);
    await mountApp(root, populated());
    await new Promise((r) => setTimeout(r, 5));
    // Live redirects to Pulse with the Now feed selected. No 404 or dead route remains.
    expect(location.hash).toBe("#/pulse?mode=now");
    expect(root.querySelector(".main .pulse")).toBeTruthy();
    expect(root.querySelector(".main .pulse-now")).toBeTruthy();
    root.remove();
    location.hash = "";
  });
});

describe("today-spend polling", () => {
  beforeEach(() => setOnboarded(true));

  it("installs a ~30s interval that refreshes today-spend while the app is open", async () => {
    location.hash = "#/pulse";
    let todayCalls = 0;
    const client = populated();
    const origToday = client.today.bind(client);
    client.today = async () => {
      todayCalls++;
      return origToday();
    };

    // Capture the intervals the shell installs (rather than waiting real wall-clock). Pulse's Now
    // feed installs its own poll too, so match the 30s today-spend interval by period
    // rather than assuming it's the only one.
    const realSetInterval = globalThis.setInterval;
    let tick: (() => void) | null = null;
    const intervalMsSeen: number[] = [];
    (globalThis as unknown as { setInterval: unknown }).setInterval = (
      fn: () => void,
      ms: number
    ) => {
      intervalMsSeen.push(ms);
      if (ms === 30_000) tick = fn;
      return 0;
    };
    try {
      const root = document.createElement("div");
      document.body.appendChild(root);
      await mountApp(root, client);
      await new Promise((r) => setTimeout(r, 5)); // settle the initial refresh
      expect(intervalMsSeen).toContain(30_000);
      const before = todayCalls;
      expect(before).toBeGreaterThanOrEqual(1); // fetched once on mount
      (tick as (() => void) | null)?.(); // simulate one poll tick
      await new Promise((r) => setTimeout(r, 5));
      expect(todayCalls).toBeGreaterThan(before); // and again on the interval
      // The tape's spend figure left first-load "loading" once real data arrived.
      expect(root.querySelector(".tape-spend .val.loading")).toBeNull();
      root.remove();
      location.hash = "";
    } finally {
      globalThis.setInterval = realSetInterval;
    }
  });
});

// The rail exposes only Pulse, Investigate, and Optimize as analytical destinations. Compatibility
// routes remain reachable through the palette, so this suite checks both halves of the contract.
describe("rail composition", () => {
  it("exposes exactly Pulse, Investigate and Optimize as analytical destinations", () => {
    expect(SECTIONS.flatMap((s) => s.items).map((i) => i.name)).toEqual([
      "pulse",
      "investigate",
      "optimize",
    ]);
  });

  it("exposes low-frequency capabilities only as canonical child lenses", () => {
    expect(LENS_ROUTES.map((route) => route.name)).toEqual([
      "correlations",
      "lineage",
      "units",
      "scenarios",
    ]);
    expect(LENS_ROUTES.every((route) => /^#\/(investigate|optimize)(?:\?|$)/.test(route.href))).toBe(true);
    expect(LENS_ROUTES.some((route) => ["runs", "sessions", "trends", "experiments"].includes(route.name))).toBe(false);
  });
});
