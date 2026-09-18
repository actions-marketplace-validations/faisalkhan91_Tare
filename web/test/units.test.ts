import { describe, it, expect } from "vitest";
import { renderUnits } from "../src/screens/units.js";
import { fakeClient } from "./fakeClient.js";
import type { UnitReport } from "../src/client.js";

describe("Units of work screen", () => {
  it("renders a row per unit plus the unbucketed remainder", async () => {
    const rep: UnitReport = {
      rows: [
        { name: "PR #1", runs: 2, steps: 4, tokens: 200000, cost_micros: 6_000_000, micros_per_run: 3_000_000 },
        { name: "hotfix", runs: 1, steps: 2, tokens: 50000, cost_micros: 3_000_000, micros_per_run: 3_000_000 },
        { name: "unbucketed", runs: 1, steps: 1, tokens: 10000, cost_micros: 1_000_000, micros_per_run: 1_000_000 },
      ],
      unbucketed_runs: 1,
      total_micros: 10_000_000,
      pricing_version: "2026-06-01",
      estimated: true,
    };
    const client = fakeClient({ units: async () => rep });
    const root = document.createElement("div");
    await renderUnits(root, client);
    const rows = root.querySelectorAll(".driver-row");
    expect(rows.length).toBe(3);
    expect(Array.from(rows).map((r) => r.querySelector(".driver-label")?.textContent)).toEqual([
      "PR #1",
      "hotfix",
      "unbucketed",
    ]);
    // The unbucketed row is de-emphasized (coverage honesty, visually muted).
    expect(rows[2].classList.contains("is-muted")).toBe(true);
    // The caption surfaces the total + unbucketed count.
    expect(root.textContent).toContain("$10.00 total");
    expect(root.textContent).toContain("1 run unbucketed");
  });

  it("shows an empty state (with the [[unit]] hint) when no units are configured", async () => {
    // Only the unbucketed row present → treated as "nothing declared".
    const client = fakeClient({
      units: async () => ({
        rows: [{ name: "unbucketed", runs: 3, steps: 5, tokens: 100, cost_micros: 5_000_000, micros_per_run: 1_666_666 }],
        unbucketed_runs: 3,
        total_micros: 5_000_000,
        pricing_version: "x",
        estimated: true,
      }),
    });
    const root = document.createElement("div");
    await renderUnits(root, client);
    expect(root.querySelector(".empty-state")?.textContent).toContain("No units of work configured");
    expect(root.querySelector(".empty-state")?.textContent).toContain("[[unit]]");
    // A config task edited in tare.toml → no misleading Connect CTA.
    expect(root.querySelector(".empty-state a")).toBeNull();
  });

  it("shows a visible error when the fetch fails", async () => {
    const root = document.createElement("div");
    await renderUnits(
      root,
      fakeClient({
        units: async () => {
          throw new Error("boom");
        },
      })
    );
    // Error path uses errorNode (plain-language message; raw error demoted to the title), NOT the
    // emptyState dead-end Connect CTA.
    const err = root.querySelector(".error");
    expect(err?.textContent).toContain("Couldn't load work units");
    expect(root.querySelector(".error-state button")?.textContent).toBe("Retry");
    expect(err?.getAttribute("title")).toContain("boom");
    expect(root.querySelector(".empty-state")).toBeNull();
  });
});
