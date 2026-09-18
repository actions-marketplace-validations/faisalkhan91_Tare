// Flamegraph view transforms used by the bounded Run Profile. The sub-pixel culler reshapes the
// on-screen model before rendering, leaving the byte-stable export renderer and its golden untouched.

import { flameWeightOf, type FlameWeight, type FlamegraphModel, type FlamegraphNode } from "../svg.js";

/// The single heaviest child by `weight`; ties break by name for deterministic output.
function heaviestChild(children: FlamegraphNode[], weight: FlameWeight): FlamegraphNode {
  return children.reduce((a, b) =>
    flameWeightOf(b, weight) > flameWeightOf(a, weight) ||
    (flameWeightOf(b, weight) === flameWeightOf(a, weight) && b.name < a.name)
      ? b
      : a
  );
}

/// Filter by the visible-width floor. When every child is below the threshold, retaining the
/// heaviest positive-weight child keeps a useful path visible while adding at most one node per tree
/// depth. An all-zero branch has no visible path and is removed entirely.
function filterOrKeepHeaviest(
  children: FlamegraphNode[],
  minWeight: number,
  weight: FlameWeight
): FlamegraphNode[] {
  const kept = children.filter((child) => flameWeightOf(child, weight) >= minWeight);
  if (kept.length > 0 || children.length === 0) return kept;
  const heaviest = heaviestChild(children, weight);
  return flameWeightOf(heaviest, weight) > 0 ? [heaviest] : [];
}

function cullIn(node: FlamegraphNode, minWeight: number, weight: FlameWeight): FlamegraphNode {
  const kept = filterOrKeepHeaviest(node.children, minWeight, weight);
  let changed = kept.length !== node.children.length;
  const children = kept.map((child) => {
    const culled = cullIn(child, minWeight, weight);
    changed ||= culled !== child;
    return culled;
  });
  return changed ? { ...node, children } : node;
}

/// Drop frames too narrow to become visible DOM nodes. Rendered width is proportional to
/// `flameWeightOf(node, weight) / flameWeightOf(root, weight)`, so the minimum visible weight is
/// derived directly from the canvas width. `weight` must match the renderer's width mode.
///
/// Returns the original model when no culling is needed; otherwise returns a new model and leaves the
/// input untouched.
export function cullSubPixel(
  model: FlamegraphModel,
  minWidthPx = 1,
  renderWidthPx = 960,
  weight: FlameWeight = "tokens"
): FlamegraphModel {
  const total = flameWeightOf(model.root, weight);
  if (total <= 0 || minWidthPx <= 0 || renderWidthPx <= 0) return model;
  const minWeight = Math.ceil((total * minWidthPx) / renderWidthPx);
  const root = cullIn(model.root, minWeight, weight);
  return root === model.root ? model : { ...model, root };
}
