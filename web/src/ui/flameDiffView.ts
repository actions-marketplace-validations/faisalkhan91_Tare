// Screen-only hierarchical flame-diff renderer. A DETERMINISTIC typed-tree
// view over the `FlameDiffModel` the core returns for an explicit run pair — NOT the Rust/export SVG
// renderer, whose golden output is untouched. The tree preserves the backend's child order (already
// sorted by weight); this module only projects, filters (diff-only), and focuses it, and renders labels
// + borders so it reads without color (forced-colors friendly). Absolute mode compares
// dollars; normalized (share) mode compares structural proportion so two different-sized runs line up.

import { el } from "./el.js";
import { fmtUsd, fmtSignedUsd } from "./format.js";
import { setRouteQuery } from "./store.js";
import type { FlameDiffModel, FlameDiffNode } from "../client.js";

export type FlameDiffMode = "absolute" | "normalized";

export interface FlameDiffOpts {
  mode: FlameDiffMode;
  diffOnly: boolean;
  /// Child-index path from the model root; [] focuses the whole tree.
  focusPath: number[];
}

export interface FlameDiffRow {
  node: FlameDiffNode;
  depth: number;
  /// Child-index path from the model root (stable identity for focus deep-links).
  path: number[];
  hasChildren: boolean;
}

// A real captured run can contain tens of thousands of structural frames. Keep each rendered page
// bounded so Compare never creates a six-figure DOM while retaining the complete deterministic row
// projection for sorting/focus semantics.
export const FLAME_DIFF_PAGE_SIZE = 250;

/// The active-mode signed delta for a node: dollars in absolute mode, share basis points in
/// normalized (structural) mode.
export function nodeDelta(node: FlameDiffNode, mode: FlameDiffMode): number {
  return mode === "normalized" ? node.delta_bps : node.delta_micros;
}

/// True when a node OR any descendant carries a non-zero delta in the active mode — so diff-only never
/// hides a subtree whose leaves changed even if the parent's own delta is zero.
export function nodeChanged(node: FlameDiffNode, mode: FlameDiffMode): boolean {
  return nodeDelta(node, mode) !== 0 || node.children.some((child) => nodeChanged(child, mode));
}

/// Resolve a focus path to its subtree root. An out-of-range segment stops resolution at the last
/// valid node (a stale deep-link focuses the deepest still-valid ancestor rather than throwing).
export function focusNode(model: FlameDiffModel, focusPath: number[]): { node: FlameDiffNode; path: number[] } {
  let node = model.root;
  const resolved: number[] = [];
  for (const index of focusPath) {
    const child = node.children[index];
    if (!child) break;
    node = child;
    resolved.push(index);
  }
  return { node, path: resolved };
}

/// Flatten the (focused) tree into ordered rows for rendering. Backend child order is preserved (it is
/// already the deterministic weight sort); diff-only prunes unchanged subtrees. The focused root is
/// always row 0 so the view has a stable header even when its own delta is zero.
export function flameDiffRows(model: FlameDiffModel, opts: FlameDiffOpts): FlameDiffRow[] {
  const { node: rootNode, path: rootPath } = focusNode(model, opts.focusPath);
  const rows: FlameDiffRow[] = [
    { node: rootNode, depth: 0, path: rootPath, hasChildren: rootNode.children.length > 0 },
  ];
  const walk = (node: FlameDiffNode, depth: number, path: number[]): void => {
    node.children.forEach((child, index) => {
      if (opts.diffOnly && !nodeChanged(child, opts.mode)) return;
      const childPath = [...path, index];
      rows.push({ node: child, depth, path: childPath, hasChildren: child.children.length > 0 });
      walk(child, depth + 1, childPath);
    });
  };
  walk(rootNode, 1, rootPath);
  return rows;
}

/// Share basis points → percent string (10000 bps = 100.00%).
function fmtBps(bps: number): string {
  return `${(bps / 100).toFixed(2)}%`;
}

function fmtSignedBps(bps: number): string {
  return (bps < 0 ? "-" : "+") + fmtBps(Math.abs(bps));
}

/// Human direction for a node's active-mode delta. Units stay explicit; a zero delta reads "no change"
/// so the sign is never implied.
function directionText(delta: number, mode: FlameDiffMode): string {
  if (delta === 0) return "no change";
  if (mode === "normalized") return delta > 0 ? "larger share" : "smaller share";
  return delta > 0 ? "costlier" : "cheaper";
}

/// Build the flame-diff tree view. Interaction (mode toggle, diff-only, focus/reset) is expressed
/// through the route query so it is deep-linkable and deterministic; `flameDiffRows` above is the
/// testable core. `focusPrefix` is the query key namespace so more than one instance can't collide.
export function flameDiffTree(model: FlameDiffModel, opts: FlameDiffOpts): HTMLElement {
  const { node: focused, path: focusedPath } = focusNode(model, opts.focusPath);
  const rows = flameDiffRows(model, opts);

  const modeToggle = el("div", { class: "flame-diff-modes", role: "group", "aria-label": "Flame diff mode" }, [
    modeButton("Absolute $", "absolute", opts.mode),
    modeButton("Normalized (share)", "normalized", opts.mode),
  ]);
  const diffOnly = el("label", { class: "flame-diff-diffonly" }, [
    (() => {
      const box = el("input", { type: "checkbox", ...(opts.diffOnly ? { checked: "" } : {}) }) as HTMLInputElement;
      box.addEventListener("change", () => setRouteQuery({ flame_diff_only: box.checked ? "1" : "" }));
      return box;
    })(),
    el("span", { text: "Changed frames only" }),
  ]);

  // Focus breadcrumb: reset-to-root when focused below the model root.
  const focusBar = focusedPath.length > 0
    ? el("p", { class: "flame-diff-focus caption", role: "status" }, [
        el("span", { text: `Focused on ${focused.name}. ` }),
        (() => {
          const reset = el("button", { type: "button", class: "linklike", text: "Show whole tree" });
          reset.addEventListener("click", () => setRouteQuery({ flame_focus: "" }));
          return reset;
        })(),
      ])
    : null;

  const header = el("tr", {}, [
    el("th", { text: "Frame" }),
    el("th", { class: "num", text: opts.mode === "normalized" ? `${model.run_a} share` : `${model.run_a} $` }),
    el("th", { class: "num", text: opts.mode === "normalized" ? `${model.run_b} share` : `${model.run_b} $` }),
    el("th", { class: "num", text: "Δ" }),
    el("th", { text: "Direction" }),
  ]);

  const renderRow = (row: FlameDiffRow, globalIndex?: number): HTMLElement => {
    const delta = nodeDelta(row.node, opts.mode);
    const before = opts.mode === "normalized" ? fmtBps(row.node.share_a_bps) : fmtUsd(row.node.micros_a);
    const after = opts.mode === "normalized" ? fmtBps(row.node.share_b_bps) : fmtUsd(row.node.micros_b);
    const deltaText = opts.mode === "normalized" ? fmtSignedBps(delta) : fmtSignedUsd(delta);
    // The frame name is a focus button when it has children; a leaf is plain text. Indentation encodes
    // depth visually and hidden text exposes it to AT; aria-level is not valid on a table row header.
    const label = row.hasChildren
      ? (() => {
          const btn = el("button", { type: "button", class: "flame-diff-name linklike", text: row.node.name });
          btn.addEventListener("click", () => setRouteQuery({ flame_focus: row.path.join(".") }));
          return btn;
        })()
      : el("span", { class: "flame-diff-name", text: row.node.name });
    const frameCell = el("th", {
      scope: "row",
      class: "flame-diff-frame",
      style: `padding-left: calc(${row.depth} * var(--s-4))`,
    }, [
      el("span", { class: "sr-only", text: `Level ${row.depth + 1}. ` }),
      label,
      ...(row.node.cache_class ? [el("span", { class: "flame-diff-cache badge", text: row.node.cache_class })] : []),
    ]);
    return el("tr", {
      class: delta === 0 ? "flame-diff-flat" : delta > 0 ? "flame-diff-up" : "flame-diff-down",
      "data-path": row.path.join("."),
      ...(globalIndex !== undefined ? { "aria-rowindex": String(globalIndex + 2) } : {}),
      ...(delta !== 0 ? { "data-changed": "true" } : {}),
    }, [
      frameCell,
      el("td", { class: "num", text: before }),
      el("td", { class: "num", text: after }),
      el("td", { class: "num dollars", text: deltaText }),
      el("td", { text: directionText(delta, opts.mode) }),
    ]);
  };

  const paginated = rows.length > FLAME_DIFF_PAGE_SIZE;
  const tbody = el("tbody", {});
  const table = el("table", {
    class: "data flame-diff-table",
    ...(paginated ? { "aria-rowcount": String(rows.length + 1) } : {}),
  }, [
    el("thead", {}, [header]),
    tbody,
  ]);
  const tableScroll = el("div", { class: "table-scroll" }, [table]);
  let pagination: HTMLElement | null = null;
  if (!paginated) {
    tbody.replaceChildren(...rows.map((row) => renderRow(row)));
  } else {
    header.setAttribute("aria-rowindex", "1");
    const pageCount = Math.ceil(rows.length / FLAME_DIFF_PAGE_SIZE);
    let page = 0;
    const previous = el("button", {
      class: "btn ghost",
      type: "button",
      text: "Previous",
    }) as HTMLButtonElement;
    const next = el("button", {
      class: "btn ghost",
      type: "button",
      text: "Next",
    }) as HTMLButtonElement;
    const pageInput = el("input", {
      class: "flame-diff-page-input",
      type: "number",
      min: "1",
      max: String(pageCount),
      step: "1",
      value: "1",
      "aria-label": "Structural flame diff page",
    }) as HTMLInputElement;
    const status = el("span", {
      class: "flame-diff-page-status",
      role: "status",
      "aria-live": "polite",
      "aria-atomic": "true",
    });
    const renderPage = (moveToStart = false): void => {
      const start = page * FLAME_DIFF_PAGE_SIZE;
      const end = Math.min(rows.length, start + FLAME_DIFF_PAGE_SIZE);
      tbody.replaceChildren(
        ...rows.slice(start, end).map((row, index) => renderRow(row, start + index))
      );
      previous.disabled = page === 0;
      next.disabled = page === pageCount - 1;
      pageInput.value = String(page + 1);
      status.textContent = `Rows ${start + 1}–${end} of ${rows.length}`;
      if (moveToStart) tableScroll.scrollIntoView?.({ block: "start", inline: "nearest" });
    };
    const goToPage = (value: number): void => {
      page = Math.max(0, Math.min(pageCount - 1, value));
      renderPage(true);
    };
    previous.addEventListener("click", () => goToPage(page - 1));
    next.addEventListener("click", () => goToPage(page + 1));
    const commitPageInput = (): void => {
      const requested = Number.parseInt(pageInput.value, 10);
      if (!Number.isFinite(requested)) {
        pageInput.value = String(page + 1);
        return;
      }
      goToPage(requested - 1);
    };
    pageInput.addEventListener("change", commitPageInput);
    pageInput.addEventListener("keydown", (event) => {
      if (event.key === "Enter") commitPageInput();
    });
    pagination = el("nav", {
      class: "flame-diff-pagination",
      "aria-label": "Structural flame diff pages",
    }, [
      status,
      previous,
      el("label", { class: "flame-diff-page-label" }, [
        el("span", { text: "Page" }),
        pageInput,
        el("span", { text: `of ${pageCount}` }),
      ]),
      next,
    ]);
    renderPage();
  }

  return el("section", {
    class: "section flame-diff",
    "data-mode": opts.mode,
    "data-diff-only": String(opts.diffOnly),
    "data-pair": `${model.run_a}→${model.run_b}`,
  }, [
    el("h2", { text: "Structural flame diff" }),
    el("p", {
      class: "caption",
      text: opts.mode === "normalized"
        ? `Structural comparison of ${model.run_a} → ${model.run_b} by SHARE of each run — proportions, so two different-sized runs line up. Totals: ${fmtUsd(model.total_a_micros)} → ${fmtUsd(model.total_b_micros)} (estimated). Run-pair only; never a synthesized aggregate tree.`
        : `Node-level dollar comparison of ${model.run_a} → ${model.run_b}. Totals: ${fmtUsd(model.total_a_micros)} → ${fmtUsd(model.total_b_micros)} (estimated). Run-pair only; never a synthesized aggregate tree.`,
    }),
    el("div", { class: "flame-diff-controls" }, [modeToggle, diffOnly]),
    ...(focusBar ? [focusBar] : []),
    ...(pagination ? [pagination] : []),
    tableScroll,
  ]);
}

function modeButton(label: string, mode: FlameDiffMode, active: FlameDiffMode): HTMLElement {
  const btn = el("button", {
    type: "button",
    class: mode === active ? "active" : "",
    "aria-pressed": String(mode === active),
    text: label,
  });
  // Switching mode re-fetches (normalized changes the backend diff), so clear any focus that may not
  // exist in the other projection — honest reset rather than a dangling focus path.
  btn.addEventListener("click", () => setRouteQuery({ flame_mode: mode, flame_focus: "" }));
  return btn;
}
