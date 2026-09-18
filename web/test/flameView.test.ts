import { describe, it, expect } from "vitest";
import { cullSubPixel } from "../src/ui/flameView.js";
import type { FlamegraphModel, FlamegraphNode } from "../src/svg.js";

function node(name: string, tokens: number, children: FlamegraphNode[] = []): FlamegraphNode {
  return { name, tokens, micros: tokens, children };
}

function model(root: FlamegraphNode): FlamegraphModel {
  return { run_id: "r", pricing_version: "v", effective_date: "2026-06-01", root };
}

describe("cullSubPixel", () => {
  // Root 9,600 tokens over 960px means a frame needs at least 10 tokens to reach one pixel.
  it("drops sub-pixel frames and their subtrees", () => {
    const input = model(
      node("run", 9_600, [
        node("big", 9_000, [node("big leaf", 9_000)]),
        node("mid", 591),
        node("tiny", 9, [node("tiny leaf", 9)]),
      ])
    );
    const output = cullSubPixel(input);
    expect(output.root.children.map((child) => child.name)).toEqual(["big", "mid"]);
    expect(output.root.children[0].children.map((child) => child.name)).toEqual(["big leaf"]);
  });

  it("drops a sub-pixel descendant under a visible parent", () => {
    const input = model(node("run", 9_600, [node("big", 9_600, [node("keep", 9_591), node("drop", 9)])]));
    expect(cullSubPixel(input).root.children[0].children.map((child) => child.name)).toEqual(["keep"]);
  });

  it("returns the same model when no frame needs culling", () => {
    const input = model(node("run", 100, [node("a", 60), node("b", 40)]));
    expect(cullSubPixel(input)).toBe(input);
  });

  it("drops zero-width descendants when the visible threshold rounds to one", () => {
    const input = model(node("run", 100, [node("work", 100, [node("visible", 100), node("zero", 0)])]));
    const output = cullSubPixel(input);
    expect(output.root.children[0].children.map((child) => child.name)).toEqual(["visible"]);
  });

  it("does not retain a zero-width child as the heaviest fallback", () => {
    const input = model(node("run", 100, [node("work", 100, [node("zero a", 0), node("zero b", 0)])]));
    expect(cullSubPixel(input).root.children[0].children).toEqual([]);
  });

  it("returns the same model for a zero-weight root", () => {
    const input = model(node("run", 0));
    expect(cullSubPixel(input)).toBe(input);
  });

  it("does not mutate the input", () => {
    const input = model(node("run", 9_600, [node("big", 9_591), node("tiny", 9)]));
    cullSubPixel(input);
    expect(input.root.children).toHaveLength(2);
  });

  it("keeps the heaviest child when every child is sub-pixel", () => {
    const children = Array.from({ length: 1_000 }, (_, index) => node(`step ${index}`, 1));
    children[42] = node("step 42", 3);
    const output = cullSubPixel(model(node("run", 1_000_000, children)));
    expect(output.root.children.map((child) => child.name)).toEqual(["step 42"]);
  });

  it("keeps the heaviest descendant below a retained parent", () => {
    const leaves = Array.from({ length: 500 }, (_, index) => node(`leaf ${index}`, 1));
    leaves[10] = node("leaf 10", 2);
    const input = model(node("run", 1_000_000, [node("big", 5_000, leaves), node("mid", 995_000)]));
    const big = cullSubPixel(input).root.children.find((child) => child.name === "big");
    expect(big?.children.map((child) => child.name)).toEqual(["leaf 10"]);
  });

  it("uses the renderer's active weight rather than always using tokens", () => {
    const expensive = { ...node("expensive", 1), micros: 9_000 };
    const tokenHeavy = { ...node("token heavy", 9_000), micros: 1 };
    const root = { ...node("run", 9_001, [expensive, tokenHeavy]), micros: 9_001 };
    const output = cullSubPixel(model(root), 1, 1, "cost");
    expect(output.root.children.map((child) => child.name)).toEqual(["expensive"]);
  });
});
