import { describe, expect, it } from "vitest";
import type { FlamegraphModel, FlamegraphNode } from "../src/svg.js";
import {
  aggregatedProfile,
  applyFrameAction,
  chronologicalProfile,
  componentChoices,
  profileFrames,
  profileTableRows,
  sandwichProfile,
} from "../src/workspaces/runProfileModel.js";

function leaf(name: string, tokens: number, micros: number, cache_class: string): FlamegraphNode {
  return { name, tokens, micros, cache_class, children: [] };
}

function component(name: string, children: FlamegraphNode[]): FlamegraphNode {
  return {
    name,
    tokens: children.reduce((sum, child) => sum + child.tokens, 0),
    micros: children.reduce((sum, child) => sum + child.micros, 0),
    children,
  };
}

function step(ordinal: number, model: string, children: FlamegraphNode[]): FlamegraphNode {
  return {
    name: `step ${ordinal} · ${model}`,
    tokens: children.reduce((sum, child) => sum + child.tokens, 0),
    micros: children.reduce((sum, child) => sum + child.micros, 0),
    children,
  };
}

function fixture(): FlamegraphModel {
  const children = [
    step(2, "model-a", [
      component("System", [leaf("fresh", 40, 400, "fresh")]),
      component("Tools", [leaf("output", 20, 100, "output")]),
    ]),
    step(1, "model-a", [
      component("System", [leaf("fresh", 30, 300, "fresh")]),
      component("Tools", [leaf("output", 80, 500, "output")]),
    ]),
    step(3, "model-b", [
      component("System", [leaf("cache read", 200, 50, "cache_read")]),
    ]),
  ];
  return {
    run_id: "run-z",
    pricing_version: "test",
    effective_date: "2026-07-01",
    root: {
      name: "run run-z",
      tokens: children.reduce((sum, child) => sum + child.tokens, 0),
      micros: children.reduce((sum, child) => sum + child.micros, 0),
      children,
    },
  };
}

describe("Run Profile screen models", () => {
  it("keeps chronological step/component order and exact step references", () => {
    const model = chronologicalProfile(fixture());
    expect(model.root.children.map((child) => child.name)).toEqual([
      "step 2 · model-a",
      "step 1 · model-a",
      "step 3 · model-b",
    ]);
    expect(model.root.children[0].children.map((child) => child.name)).toEqual(["System", "Tools"]);
    expect(model.root.children[0].children[0].references).toEqual([{ run_id: "run-z", step_ordinal: 2 }]);
  });

  it("recursively merges equal FrameKeys within a path instead of merely sorting siblings", () => {
    const model = aggregatedProfile(fixture(), "cost");
    expect(model.root.children).toHaveLength(2);
    const modelA = model.root.children.find((child) => child.normalized_label === "model-a")!;
    const modelB = model.root.children.find((child) => child.normalized_label === "model-b")!;
    expect(modelA.calls).toBe(2);
    expect(modelA.micros).toBe(1_300);
    expect(modelA.references).toEqual([
      { run_id: "run-z", step_ordinal: 1 },
      { run_id: "run-z", step_ordinal: 2 },
    ]);
    expect(modelA.children.map((child) => [child.display_label, child.calls, child.micros])).toEqual([
      ["System", 2, 700],
      ["Tools", 2, 600],
    ]);
    // Same-named System under model-b remains on its distinct full path.
    expect(modelB.children).toHaveLength(1);
    expect(modelB.children[0].display_label).toBe("System");
    expect(modelB.children[0].calls).toBe(1);
  });

  it("sorts every aggregated level by the active Cost/Tokens weight with stable ties", () => {
    const cost = aggregatedProfile(fixture(), "cost");
    const tokens = aggregatedProfile(fixture(), "tokens");
    expect(cost.root.children[0].display_label).toBe("model-a");
    expect(tokens.root.children[0].display_label).toBe("model-b");
    const costA = cost.root.children.find((child) => child.display_label === "model-a")!;
    const tokenA = tokens.root.children.find((child) => child.display_label === "model-a")!;
    expect(costA.children.map((child) => child.display_label)).toEqual(["System", "Tools"]);
    expect(tokenA.children.map((child) => child.display_label)).toEqual(["Tools", "System"]);
  });

  it("builds a selected-component caller/selected/callee sandwich", () => {
    const system = componentChoices(fixture()).find((choice) => choice.label === "System")!;
    expect(system.calls).toBe(3);
    const model = sandwichProfile(fixture(), system.frame_key, "cost")!;
    expect(model.root.micros).toBe(750);
    expect(model.root.children.map((caller) => caller.display_label)).toEqual(["model-a", "model-b"]);
    expect(model.root.children[0].children).toHaveLength(1);
    expect(model.root.children[0].children[0].display_label).toBe("System");
    const rows = profileTableRows(model, "cost", "cumulative", "", true);
    expect(rows.some((row) => row.role === "caller" && row.label === "model-a")).toBe(true);
    expect(rows.some((row) => row.role === "selected" && row.label === "System")).toBe(true);
    expect(rows.some((row) => row.role === "callee" && row.node_kind === "cache_class")).toBe(true);
  });

  it("supports focus, ignore, hide, search, flat/cumulative math, calls and cost/call", () => {
    const model = aggregatedProfile(fixture(), "cost");
    const system = profileFrames(model).find((frame) => frame.node_kind === "component" && frame.display_label === "System")!;
    const focused = applyFrameAction(model, "focus", system.frame_key);
    expect(focused.root.children.length).toBeGreaterThan(0);
    expect(focused.root.micros).toBe(750);
    expect(profileFrames(applyFrameAction(model, "ignore", system.frame_key)).some((frame) => frame.frame_key === system.frame_key)).toBe(false);
    expect(profileFrames(applyFrameAction(model, "hide", system.frame_key)).some((frame) => frame.frame_key === system.frame_key)).toBe(false);

    const cumulative = profileTableRows(model, "cost", "cumulative", "tools");
    expect(cumulative).toHaveLength(1);
    expect(cumulative[0]).toMatchObject({ calls: 2, cum_micros: 600, cost_per_call_micros: 300 });
    const flat = profileTableRows(model, "tokens", "flat");
    expect(flat[0].node_kind).toBe("cache_class");
  });

  it("stays fast on a run with many steps sharing one recurring component", () => {
    // componentChoices used to call mergeReferences INSIDE its accumulation loop, rebuilding +
    // sorting the whole accumulated reference list on every merge — O(n²) for a component label
    // repeated across many steps. A real 54k-step run hung the browser for 5+ minutes; this
    // synthetic 6k-step run (one shared "System" component per step, matching that real shape)
    // should complete in well under a second on the fixed O(n log n) implementation.
    const STEP_COUNT = 6000;
    const children: FlamegraphNode[] = [];
    for (let i = 1; i <= STEP_COUNT; i++) {
      children.push(
        step(i, "model-a", [component("System", [leaf("fresh", 10, 100, "fresh")])])
      );
    }
    const model: FlamegraphModel = {
      run_id: "run-big",
      pricing_version: "test",
      effective_date: "2026-07-01",
      root: {
        name: "run run-big",
        tokens: children.reduce((sum, child) => sum + child.tokens, 0),
        micros: children.reduce((sum, child) => sum + child.micros, 0),
        children,
      },
    };
    const t0 = Date.now();
    const choices = componentChoices(model);
    const elapsedMs = Date.now() - t0;
    const system = choices.find((choice) => choice.label === "System")!;
    expect(system.calls).toBe(STEP_COUNT);
    expect(system.references).toHaveLength(STEP_COUNT);
    expect(elapsedMs).toBeLessThan(1000);
  });
});
