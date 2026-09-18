// Flame-diff typed-tree renderer. The pure projection (rows/diff-only/focus) is the
// testable core; the DOM builder proves labels, direction, units, and forced-color-friendly structure.
import { describe, it, expect } from "vitest";
import {
  flameDiffRows,
  nodeChanged,
  nodeDelta,
  focusNode,
  flameDiffTree,
  FLAME_DIFF_PAGE_SIZE,
  type FlameDiffOpts,
} from "../src/ui/flameDiffView.js";
import type { FlameDiffModel, FlameDiffNode } from "../src/client.js";

function node(name: string, over: Partial<FlameDiffNode>): FlameDiffNode {
  return {
    name,
    tokens_a: 0,
    tokens_b: 0,
    micros_a: 0,
    micros_b: 0,
    delta_micros: 0,
    share_a_bps: 0,
    share_b_bps: 0,
    delta_bps: 0,
    children: [],
    ...over,
  };
}

// base 10.00 → cand 15.00: system holds dollars (share shifts −20pp), tools rises +5.00 via `search`.
const MODEL: FlameDiffModel = {
  run_a: "base",
  run_b: "cand",
  normalized: false,
  total_a_micros: 10_000_000,
  total_b_micros: 15_000_000,
  root: node("root", {
    micros_a: 10_000_000,
    micros_b: 15_000_000,
    delta_micros: 5_000_000,
    share_a_bps: 10_000,
    share_b_bps: 10_000,
    delta_bps: 0,
    children: [
      node("system", {
        micros_a: 6_000_000,
        micros_b: 6_000_000,
        delta_micros: 0,
        share_a_bps: 6_000,
        share_b_bps: 4_000,
        delta_bps: -2_000,
      }),
      node("tools", {
        micros_a: 4_000_000,
        micros_b: 9_000_000,
        delta_micros: 5_000_000,
        share_a_bps: 4_000,
        share_b_bps: 6_000,
        delta_bps: 2_000,
        cache_class: "fresh",
        children: [
          node("search", { micros_a: 4_000_000, micros_b: 9_000_000, delta_micros: 5_000_000, share_a_bps: 0, share_b_bps: 0, delta_bps: 0 }),
        ],
      }),
    ],
  }),
};

const OPTS = (over: Partial<FlameDiffOpts> = {}): FlameDiffOpts => ({ mode: "absolute", diffOnly: false, focusPath: [], ...over });

describe("flame-diff projection", () => {
  it("nodeDelta reads dollars in absolute mode and share bps in normalized mode", () => {
    const system = MODEL.root.children[0];
    expect(nodeDelta(system, "absolute")).toBe(0);
    expect(nodeDelta(system, "normalized")).toBe(-2_000);
  });

  it("nodeChanged follows the active mode (system moved only in share, not dollars)", () => {
    const system = MODEL.root.children[0];
    expect(nodeChanged(system, "absolute")).toBe(false);
    expect(nodeChanged(system, "normalized")).toBe(true);
    // tools changed in dollars via its `search` child even though we check the parent.
    expect(nodeChanged(MODEL.root.children[1], "absolute")).toBe(true);
  });

  it("flattens the whole tree preserving backend child order, with depths and index paths", () => {
    const rows = flameDiffRows(MODEL, OPTS());
    expect(rows.map((r) => r.node.name)).toEqual(["root", "system", "tools", "search"]);
    expect(rows.map((r) => r.depth)).toEqual([0, 1, 1, 2]);
    expect(rows.find((r) => r.node.name === "search")?.path).toEqual([1, 0]);
  });

  it("diff-only prunes unchanged subtrees per mode (root always kept as the header)", () => {
    // Absolute: system (Δ$0, no changed child) is pruned; tools + search stay.
    expect(flameDiffRows(MODEL, OPTS({ diffOnly: true })).map((r) => r.node.name)).toEqual(["root", "tools", "search"]);
    // Normalized: system moved in share so it stays; leaf `search` (Δbps 0) is pruned.
    expect(flameDiffRows(MODEL, OPTS({ mode: "normalized", diffOnly: true })).map((r) => r.node.name)).toEqual(["root", "system", "tools"]);
  });

  it("focus roots the view at a child path; a stale/out-of-range path falls back to the last valid node", () => {
    expect(focusNode(MODEL, [1]).node.name).toBe("tools");
    expect(flameDiffRows(MODEL, OPTS({ focusPath: [1] })).map((r) => r.node.name)).toEqual(["tools", "search"]);
    // [1,9] has no 10th child → resolves to `tools`, not a throw.
    expect(focusNode(MODEL, [1, 9]).node.name).toBe("tools");
  });
});

describe("flame-diff renderer", () => {
  it("renders explicit direction + estimated-dollar units in absolute mode", () => {
    const view = flameDiffTree(MODEL, OPTS());
    expect(view.getAttribute("data-mode")).toBe("absolute");
    const tools = view.querySelector('tr[data-path="1"]')!;
    expect(tools.getAttribute("data-changed")).toBe("true");
    expect(tools.querySelector(".dollars")?.textContent).toBe("+$5.00");
    expect(tools.textContent).toContain("costlier");
    // A frame with children is a focus button; the cache class is a labelled badge, not color alone.
    expect(tools.querySelector("button.flame-diff-name")?.textContent).toBe("tools");
    expect(tools.querySelector(".flame-diff-cache")?.textContent).toBe("fresh");
  });

  it("shows share columns and share direction in normalized mode", () => {
    const view = flameDiffTree(MODEL, OPTS({ mode: "normalized" }));
    expect(view.getAttribute("data-mode")).toBe("normalized");
    const system = view.querySelector('tr[data-path="0"]')!;
    expect(system.querySelector(".dollars")?.textContent).toBe("-20.00%"); // −2000 bps
    expect(system.textContent).toContain("smaller share");
  });

  it("keeps a large structural diff DOM bounded and pages through every row", () => {
    const childCount = 12_500;
    const large: FlameDiffModel = {
      ...MODEL,
      root: node("root", {
        children: Array.from({ length: childCount }, (_, index) => node(`frame-${index}`, {
          micros_a: index,
          micros_b: index + 1,
          delta_micros: 1,
        })),
      }),
    };
    const view = flameDiffTree(large, OPTS());
    const table = view.querySelector("table")!;
    const pager = view.querySelector<HTMLElement>(".flame-diff-pagination")!;
    expect(pager).toBeTruthy();
    expect(table.getAttribute("aria-rowcount")).toBe(String(childCount + 2));
    expect(view.querySelectorAll("tbody tr")).toHaveLength(FLAME_DIFF_PAGE_SIZE);
    expect(view.querySelectorAll("*").length).toBeLessThan(3_000);
    expect(pager.querySelector('[role="status"]')?.textContent).toBe(
      `Rows 1–${FLAME_DIFF_PAGE_SIZE} of ${childCount + 1}`
    );

    (pager.querySelector("button:last-child") as HTMLButtonElement).click();
    const firstSecondPage = view.querySelector("tbody tr")!;
    expect(view.querySelectorAll("tbody tr")).toHaveLength(FLAME_DIFF_PAGE_SIZE);
    expect(firstSecondPage.getAttribute("data-path")).toBe(String(FLAME_DIFF_PAGE_SIZE - 1));
    expect(firstSecondPage.getAttribute("aria-rowindex")).toBe(String(FLAME_DIFF_PAGE_SIZE + 2));
    expect(pager.querySelector('[role="status"]')?.textContent).toBe(
      `Rows ${FLAME_DIFF_PAGE_SIZE + 1}–${FLAME_DIFF_PAGE_SIZE * 2} of ${childCount + 1}`
    );
  });

  it("does not add pagination markup to a small tree", () => {
    const view = flameDiffTree(MODEL, OPTS());
    expect(view.querySelector(".flame-diff-pagination")).toBeNull();
    expect(view.querySelector("table")?.hasAttribute("aria-rowcount")).toBe(false);
  });
});
