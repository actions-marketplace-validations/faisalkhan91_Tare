import { describe, it, expect } from "vitest";
import { createTauriClient } from "../src/tauriClient.js";

describe("Tauri TareClient", () => {
  it("maps each method to its command, parsing JSON-string results", async () => {
    const calls: Array<[string, unknown]> = [];
    const invoke = async (cmd: string, args?: Record<string, unknown>) => {
      calls.push([cmd, args]);
      switch (cmd) {
        case "list_runs":
          return ["r1", "r2"];
        case "today_spend":
          return { run_count: 1, total_micros: 4_210_000, pricing_version: "x", effective_date: "d" };
        case "run_status":
          return JSON.stringify({ run_id: "r1", micros: 5, steps: 2, top_cause: "c" });
        case "explain":
          return "a plain narrative"; // raw string, not JSON
        case "whatif":
          return JSON.stringify({ baseline_micros: 0, recommendations: [], estimated: true, approximate: true });
        default:
          return "{}";
      }
    };
    const c = createTauriClient(invoke);

    expect(await c.listRuns()).toEqual(["r1", "r2"]);
    expect((await c.today()).total_micros).toBe(4_210_000);
    expect((await c.runStatus("r1")).top_cause).toBe("c");
    // explain passes through unparsed.
    expect(await c.explain("r1")).toBe("a plain narrative");
    // whatif is JSON-parsed into an object.
    expect((await c.whatif(true)).approximate).toBe(true);

    // Args use camelCase (Tauri maps to the Rust snake_case params).
    expect(calls.find((x) => x[0] === "run_status")?.[1]).toEqual({ runId: "r1" });
    expect(calls.find((x) => x[0] === "whatif")?.[1]).toEqual({ crossProvider: true });
  });

  it("passes the selected burn-rate range across IPC", async () => {
    const calls: Array<[string, unknown]> = [];
    const c = createTauriClient(async (cmd, args) => {
      calls.push([cmd, args]);
      return "{}";
    });
    await c.burnrate("week");
    expect(calls).toEqual([["burnrate", { range: "week" }]]);
  });
});
