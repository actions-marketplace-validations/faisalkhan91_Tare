import { describe, it, expect } from "vitest";
import { renderCorrelate } from "../src/screens/correlate.js";
import { fakeClient } from "./fakeClient.js";
import type { CorrelationRow } from "../src/client.js";

const row = (over: Partial<CorrelationRow>): CorrelationRow => ({
  run_id: "r",
  model: "claude-opus-4-8",
  cache_control: false,
  effort: null,
  ttl: "5m",
  cost_micros: 1_000_000,
  tokens: 100_000,
  duration_ms: 0,
  steps: 3,
  ...over,
});

describe("Correlate screen", () => {
  it("plots one parcoords line per run and a spend-sorted table", async () => {
    const client = fakeClient({
      correlate: async () => ({
        rows: [
          row({ run_id: "cheap", cache_control: false, effort: "low", cost_micros: 1_000_000, tokens: 50_000 }),
          row({ run_id: "pricey", cache_control: true, effort: "high", ttl: "1h", cost_micros: 9_000_000, tokens: 900_000 }),
        ],
        pricing_version: "2026-06-01",
        estimated: true,
      }),
    });
    const root = document.createElement("div");
    await renderCorrelate(root, client);
    // A line per run in the parallel-coordinates plot.
    expect(root.querySelectorAll("svg.parcoords polyline.pc-line").length).toBe(2);
    // The costliest run carries the high-cost tone.
    const hot = Array.from(root.querySelectorAll("polyline.pc-line")).find((p) =>
      p.querySelector("title")?.textContent?.includes("pricey")
    )!;
    expect(hot.getAttribute("class")).toContain("cost-high");
    // Table is sorted by spend desc → pricey first, and links to the run.
    const firstRow = root.querySelector("table tbody tr")!;
    expect(firstRow.textContent).toContain("pricey");
    expect(firstRow.querySelector("a.run-link")?.getAttribute("href")).toBe("#/investigate/run/pricey");
    // Estimate-honesty: the caption names it estimated + surfaces the pricing version.
    expect(root.textContent).toContain("Estimated");
    expect(root.textContent).toContain("2026-06-01");
  });

  it("adds a latency axis only when some run has measured duration", async () => {
    const withLatency = fakeClient({
      correlate: async () => ({
        rows: [row({ run_id: "a", duration_ms: 1200 }), row({ run_id: "b", duration_ms: 0 })],
        pricing_version: "x",
        estimated: true,
      }),
    });
    const root = document.createElement("div");
    await renderCorrelate(root, withLatency);
    const axes = Array.from(root.querySelectorAll(".pc-axis-label")).map((t) => t.textContent);
    expect(axes).toContain("latency");
  });

  it("keyboard activation opens the same canonical Run Profile as its visible link", async () => {
    window.location.hash = "#/investigate?mode=distinguish";
    const root = document.createElement("div");
    await renderCorrelate(root, fakeClient({
      correlate: async () => ({ rows: [row({ run_id: "run/with/slash" })], pricing_version: "x", estimated: true }),
    }));
    const table = root.querySelector<HTMLElement>(".datatable")!;
    table.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    table.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(window.location.hash).toBe("#/investigate/run/run%2Fwith%2Fslash");
  });

  it("omits the latency axis when every duration is suppressed (clock-free core)", async () => {
    const noLatency = fakeClient({
      correlate: async () => ({
        rows: [row({ run_id: "a" }), row({ run_id: "b" })],
        pricing_version: "x",
        estimated: true,
      }),
    });
    const root = document.createElement("div");
    await renderCorrelate(root, noLatency);
    const axes = Array.from(root.querySelectorAll(".pc-axis-label")).map((t) => t.textContent);
    expect(axes).not.toContain("latency");
  });

  it("skips the plot for a single run but still tables it", async () => {
    const client = fakeClient({
      correlate: async () => ({ rows: [row({ run_id: "solo" })], pricing_version: "x", estimated: true }),
    });
    const root = document.createElement("div");
    await renderCorrelate(root, client);
    expect(root.querySelector("svg.parcoords")).toBeNull();
    expect(root.querySelector("table tbody tr")?.textContent).toContain("solo");
  });

  it("shows an empty state when there are no runs", async () => {
    const root = document.createElement("div");
    await renderCorrelate(root, fakeClient());
    expect(root.querySelector(".empty-state")?.textContent).toContain("No runs to correlate yet");
  });

  it("shows a visible error when the fetch fails", async () => {
    const root = document.createElement("div");
    await renderCorrelate(
      root,
      fakeClient({
        correlate: async () => {
          throw new Error("boom");
        },
      })
    );
    // Error path uses errorNode (plain-language message; raw error demoted to the title), NOT the
    // emptyState dead-end Connect CTA.
    const err = root.querySelector(".error");
    expect(err?.textContent).toContain("Couldn't load configuration correlations");
    expect(root.querySelector(".error-state button")?.textContent).toBe("Retry");
    expect(err?.getAttribute("title")).toContain("boom");
    expect(root.querySelector(".empty-state")).toBeNull();
  });
});
