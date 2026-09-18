import { describe, it, expect } from "vitest";
import { renderLineage } from "../src/screens/lineage.js";
import { fakeClient } from "./fakeClient.js";
import type { LineageReport } from "../src/client.js";

const rep = (over: Partial<LineageReport>): LineageReport => ({
  name: "checkout",
  rows: [],
  pricing_version: "2026-06-01",
  estimated: true,
  ...over,
});

describe("Lineage screen", () => {
  it("renders one row per version in declared order, flagging the cheapest", async () => {
    const client = fakeClient({
      lineages: async () => [
        rep({
          rows: [
            { label: "v1", hash: 1, runs: 2, steps: 4, tokens: 200000, cost_micros: 6_000_000, micros_per_run: 3_000_000 },
            { label: "v2", hash: 2, runs: 1, steps: 2, tokens: 50000, cost_micros: 1_500_000, micros_per_run: 1_500_000 },
            { label: "v3", hash: 3, runs: 0, steps: 0, tokens: 0, cost_micros: 0, micros_per_run: 0 },
          ],
        }),
      ],
    });
    const root = document.createElement("div");
    await renderLineage(root, client);
    const rows = root.querySelectorAll(".driver-row");
    expect(rows.length).toBe(3);
    // Declared order preserved: v1, v2, v3.
    expect(Array.from(rows).map((r) => r.querySelector(".driver-label")?.textContent)).toEqual(["v1", "v2", "v3"]);
    // v2 has the lowest $/run observed → flagged (renamed off "cheapest" which read as overall-best).
    expect(rows[1].querySelector(".driver-share")?.textContent).toContain("lowest $/run");
    expect(rows[0].querySelector(".driver-share")?.textContent).not.toContain("lowest $/run");
    // Bar and the bold number beside it encode the SAME measure ($/run), total moves to meta.
    expect(rows[0].querySelector(".driver-cost")?.textContent).toContain("/run");
    expect(rows[0].querySelector(".driver-share")?.textContent).toContain("total");
    // v3 has no runs yet → honest "not captured yet", not a fabricated $0/run win.
    expect(rows[2].querySelector(".driver-share")?.textContent).toContain("not captured yet");
    expect(rows[2].querySelector(".driver-cost")?.textContent).toBe("Not captured");
    // The section is titled by the lineage name.
    expect(root.querySelector("h2")?.textContent).toBe("checkout");
  });

  it("shows an empty state (with the [[lineage]] hint) when none are configured", async () => {
    const root = document.createElement("div");
    await renderLineage(root, fakeClient()); // default lineages() → []
    expect(root.querySelector(".empty-state")?.textContent).toContain("No lineages configured");
    expect(root.querySelector(".empty-state")?.textContent).toContain("[[lineage]]");
  });

  it("shows a visible error when the fetch fails", async () => {
    const root = document.createElement("div");
    await renderLineage(
      root,
      fakeClient({
        lineages: async () => {
          throw new Error("boom");
        },
      })
    );
    // Error path uses errorNode (plain-language message; raw error demoted to the title), NOT the
    // emptyState dead-end Connect CTA.
    const err = root.querySelector(".error");
    expect(err?.textContent).toContain("Couldn't load prompt lineages");
    expect(root.querySelector(".error-state button")?.textContent).toBe("Retry");
    expect(err?.getAttribute("title")).toContain("boom");
    expect(root.querySelector(".empty-state")).toBeNull();
  });
});
