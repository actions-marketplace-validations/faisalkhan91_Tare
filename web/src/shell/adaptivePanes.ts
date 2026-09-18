// Adaptive workbench layout state machine. The layout mode is driven by
// the workbench CONTAINER inline-size (not the viewport) so a workspace re-flows correctly inside a
// resized window or a Windows Snap column. This module is the single source of truth for the mode
// boundaries; `main.ts` observes the container and stamps `data-layout` on `.shell`, and shell.css /
// app.css express each mode. Pure `layoutMode` is unit-tested at representative widths (330/420/760/1100/
// 1440); the ResizeObserver wiring is a thin controller.

import { isEditableTarget } from "../commands/contexts.js";
import type { AnalysisStore } from "../analysis/store.js";

/// The workbench layout mode:
/// - `full`        (>=1280): rail + entity/facet pane + canvas + inspector.
/// - `rail-icons`  (900–1279): rail collapsed to icons; entity + canvas; inspector is Peek.
/// - `stacked`     (600–899): rail behind a workspace switcher; entity OR canvas with a back stack;
///                            inspector is Peek.
/// - `single`      (<600): one pane at a time; the title names the level and exposes Back. Stays
///                         functional down to ~330px effective (Windows Snap).
export type LayoutMode = "full" | "rail-icons" | "stacked" | "single";

/// Boundary constants (px, container inline-size). Exported so tests and CSS stay in lockstep.
export const LAYOUT_BREAKPOINTS = { full: 1280, railIcons: 900, stacked: 600 } as const;

/// Map a container inline-size to its layout mode. Never returns a "horizontal route strip" mode —
/// the rail collapses to icons then hides behind a switcher; it never scrolls horizontally.
export function layoutMode(inlineSize: number): LayoutMode {
  if (inlineSize >= LAYOUT_BREAKPOINTS.full) return "full";
  if (inlineSize >= LAYOUT_BREAKPOINTS.railIcons) return "rail-icons";
  if (inlineSize >= LAYOUT_BREAKPOINTS.stacked) return "stacked";
  return "single";
}

/// Whether the inspector is a Space-triggered Peek overlay at the two medium widths. In `full` it
/// is persistent; in `single` it is an ordinary level in the one-pane back/forward stack.
export function inspectorIsPeek(mode: LayoutMode): boolean {
  return mode === "rail-icons" || mode === "stacked";
}

/// Observe `container`'s inline-size and invoke `onMode` whenever the layout mode changes (deduped).
/// Returns a disposer. Falls back to a one-shot measurement when `ResizeObserver` is unavailable
/// (e.g. jsdom) so the initial mode is always applied. `read` lets tests inject a size.
export function observeWorkbench(
  container: HTMLElement,
  onMode: (mode: LayoutMode) => void,
  read: (el: HTMLElement) => number = (el) => el.clientWidth
): () => void {
  let last: LayoutMode | null = null;
  const apply = (): void => {
    const mode = layoutMode(read(container) || LAYOUT_BREAKPOINTS.full);
    if (mode !== last) {
      last = mode;
      onMode(mode);
    }
  };
  apply(); // initial mode, even without ResizeObserver
  const RO = (globalThis as { ResizeObserver?: typeof ResizeObserver }).ResizeObserver;
  if (!RO) return () => {};
  const ro = new RO(() => apply());
  ro.observe(container);
  return () => ro.disconnect();
}

type PaneName = "entities" | "canvas" | "inspector";
type ResizablePane = Exclude<PaneName, "canvas">;
type PaneStorage = Pick<Storage, "getItem" | "setItem">;

const PANE_WIDTHS: Record<ResizablePane, { min: number; max: number; initial: number; variable: string }> = {
  entities: { min: 260, max: 320, initial: 280, variable: "--entity-pane-w" },
  inspector: { min: 280, max: 340, initial: 300, variable: "--inspector-pane-w" },
};
const WIDTH_KEY: Record<ResizablePane, string> = {
  entities: "tare:pane-width:entities",
  inspector: "tare:pane-width:inspector",
};
const PANE_LABEL: Record<PaneName, string> = {
  entities: "Filters",
  canvas: "Results",
  inspector: "Explain",
};
const NARROW_LEVELS: Record<"stacked" | "single", PaneName[]> = {
  stacked: ["entities", "canvas"],
  single: ["entities", "canvas", "inspector"],
};

export interface AdaptivePaneOptions {
  analysis?: AnalysisStore;
  storage?: PaneStorage;
  /// The pane a deep-linked nested workspace should reveal first in the narrow back stack. Base
  /// Investigate starts on Entities; a selected Run Profile starts on Canvas; a facets link starts
  /// on Inspector (as a Peek in the two medium layouts) so its promised explanation is visible.
  initialPane?: PaneName;
}

export interface AdaptivePaneController {
  dispose(): void;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.round(value)));
}

function defaultStorage(): PaneStorage | undefined {
  try {
    return window.localStorage;
  } catch {
    // Storage can be unavailable in a hardened WebView/private browser. Resizing still works for the
    // session; persistence is a best-effort local preference and never crosses the local boundary.
    return undefined;
  }
}

function modeFromRoot(root: HTMLElement): LayoutMode {
  const value = root.closest<HTMLElement>(".shell[data-layout]")?.dataset.layout;
  return value === "full" || value === "rail-icons" || value === "stacked" || value === "single"
    ? value
    : "full";
}

function nativeSpaceTarget(target: EventTarget | null): boolean {
  const node = target instanceof Element ? target : null;
  return !!node?.closest("a,button,select,summary,[role=button],[role=link],[role=separator]");
}

/// Install the real-pane gestures. The controller owns only DOM state
/// beneath `root`, plus one document key listener while the workspace is mounted so unscoped Space
/// works when focus is on the page rather than a pane. `tare:dispose` tears that listener down on
/// route changes; detached/unit roots keep all keyboard handling local to the root.
export function installAdaptivePaneInteractions(
  root: HTMLElement,
  options: AdaptivePaneOptions = {}
): AdaptivePaneController {
  const analysis = options.analysis;
  const storage = options.storage ?? defaultStorage();
  const panes = {
    entities: root.querySelector<HTMLElement>('[data-pane="entities"]'),
    canvas: root.querySelector<HTMLElement>('[data-pane="canvas"]'),
    inspector: root.querySelector<HTMLElement>('[data-pane="inspector"]'),
  };
  const separators = {
    entities: root.querySelector<HTMLElement>('[data-resize-pane="entities"]'),
    inspector: root.querySelector<HTMLElement>('[data-resize-pane="inspector"]'),
  };
  const nav = root.querySelector<HTMLElement>("[data-pane-nav]");
  const back = root.querySelector<HTMLButtonElement>("[data-pane-back]");
  const forward = root.querySelector<HTMLButtonElement>("[data-pane-forward]");
  const title = root.querySelector<HTMLElement>("[data-pane-title]");
  const inspectorTriggers = Array.from(
    root.querySelectorAll<HTMLButtonElement>("[data-pane-inspector]")
  );
  const widths: Record<ResizablePane, number> = {
    entities: PANE_WIDTHS.entities.initial,
    inspector: PANE_WIDTHS.inspector.initial,
  };
  let mode = modeFromRoot(root);
  const initialLevels = mode === "single" ? NARROW_LEVELS.single : NARROW_LEVELS.stacked;
  let level = Math.max(0, options.initialPane ? initialLevels.indexOf(options.initialPane) : 0);
  let peek = options.initialPane === "inspector" && inspectorIsPeek(mode);
  let peekReturnPane: PaneName = "canvas";
  let disposed = false;

  const saveWidth = (pane: ResizablePane): void => {
    try {
      storage?.setItem(WIDTH_KEY[pane], String(widths[pane]));
    } catch {
      /* an in-session resize remains useful when local preference writes are denied */
    }
  };
  const applyWidth = (pane: ResizablePane, value: number, persist: boolean): void => {
    const spec = PANE_WIDTHS[pane];
    widths[pane] = clamp(value, spec.min, spec.max);
    root.style.setProperty(spec.variable, `${widths[pane]}px`);
    const separator = separators[pane];
    separator?.setAttribute("aria-valuenow", String(widths[pane]));
    separator?.setAttribute("aria-valuetext", `${widths[pane]} pixels`);
    if (persist) saveWidth(pane);
  };

  for (const pane of ["entities", "inspector"] as const) {
    const spec = PANE_WIDTHS[pane];
    let restored = spec.initial;
    try {
      const raw = storage?.getItem(WIDTH_KEY[pane]);
      if (raw !== null && raw !== undefined && Number.isFinite(Number(raw))) restored = Number(raw);
    } catch {
      /* Fall back to the declared initial width when storage is unavailable. */
    }
    const separator = separators[pane];
    if (separator) {
      separator.setAttribute("role", "separator");
      separator.setAttribute("tabindex", "0");
      separator.setAttribute("aria-orientation", "vertical");
      separator.setAttribute("aria-label", `Resize ${PANE_LABEL[pane].toLowerCase()} pane`);
      separator.setAttribute("aria-valuemin", String(spec.min));
      separator.setAttribute("aria-valuemax", String(spec.max));
    }
    applyWidth(pane, restored, false);
  }

  const levels = (): PaneName[] =>
    mode === "single" ? NARROW_LEVELS.single : NARROW_LEVELS.stacked;
  const activePane = (): PaneName => levels()[Math.min(level, levels().length - 1)];
  const paneLabel = (pane: PaneName): string =>
    panes[pane]?.dataset.paneLabel || PANE_LABEL[pane];
  const visible = (pane: PaneName, show: boolean): void => {
    const element = panes[pane];
    if (!element) return;
    element.hidden = !show;
    if (show) element.removeAttribute("aria-hidden");
    else element.setAttribute("aria-hidden", "true");
  };
  const sync = (): void => {
    root.dataset.paneLayout = mode;
    root.dataset.inspectorPeek = String(peek);
    const narrow = mode === "stacked" || mode === "single";
    const active = narrow ? activePane() : "canvas";
    root.dataset.activePane = active;
    if (nav) nav.hidden = !narrow;
    if (narrow) {
      const max = levels().length - 1;
      level = Math.min(level, max);
      if (title) title.textContent = paneLabel(activePane());
      if (back) {
        back.disabled = level === 0;
        back.textContent = level > 0 ? `Back to ${paneLabel(levels()[level - 1])}` : "Back";
      }
      if (forward) {
        forward.disabled = level === max;
        forward.textContent = level < max ? `Show ${paneLabel(levels()[level + 1])}` : "Next";
      }
    }
    for (const trigger of inspectorTriggers) {
      const expanded = mode === "single" ? activePane() === "inspector" : peek;
      trigger.setAttribute("aria-expanded", String(expanded));
      trigger.textContent = expanded ? "Close explanation" : "Explain selection";
    }

    if (mode === "full") {
      visible("entities", true);
      visible("canvas", true);
      visible("inspector", true);
    } else if (mode === "rail-icons") {
      visible("entities", true);
      visible("canvas", true);
      visible("inspector", peek);
    } else if (mode === "stacked") {
      visible("entities", activePane() === "entities");
      visible("canvas", activePane() === "canvas");
      visible("inspector", peek);
    } else {
      visible("entities", activePane() === "entities");
      visible("canvas", activePane() === "canvas");
      visible("inspector", activePane() === "inspector");
    }
  };

  const setLevel = (next: number): boolean => {
    if (mode !== "stacked" && mode !== "single") return false;
    const bounded = clamp(next, 0, levels().length - 1);
    if (bounded === level) return false;
    level = bounded;
    analysis?.setFocus({ pane: activePane() });
    sync();
    return true;
  };

  const setPeek = (open: boolean): boolean => {
    if (mode !== "rail-icons" && mode !== "stacked") return false;
    if (open === peek) return false;
    if (open) {
      const focused = analysis?.get().focus.pane;
      peekReturnPane = mode === "stacked"
        ? activePane()
        : focused === "entities" || focused === "canvas"
          ? focused
          : "canvas";
      peek = true;
      analysis?.setFocus({ pane: "inspector" });
    } else {
      peek = false;
      analysis?.setFocus({ pane: peekReturnPane });
    }
    sync();
    return true;
  };

  const applyMode = (next: LayoutMode): void => {
    if (peek && next !== "rail-icons" && next !== "stacked") {
      peek = false;
      analysis?.setFocus({ pane: peekReturnPane });
    }
    mode = next;
    if (mode === "stacked") level = Math.min(level, NARROW_LEVELS.stacked.length - 1);
    if (peek) analysis?.setFocus({ pane: "inspector" });
    else if (mode === "stacked" || mode === "single") analysis?.setFocus({ pane: activePane() });
    sync();
  };

  back?.addEventListener("click", () => void setLevel(level - 1));
  forward?.addEventListener("click", () => void setLevel(level + 1));
  for (const trigger of inspectorTriggers) {
    trigger.addEventListener("click", () => {
      if (mode === "single") {
        const inspectorLevel = levels().indexOf("inspector");
        if (activePane() === "inspector") void setLevel(Math.max(0, inspectorLevel - 1));
        else void setLevel(inspectorLevel);
      } else if (mode === "rail-icons" || mode === "stacked") {
        void setPeek(!peek);
      }
    });
  }

  for (const pane of ["entities", "inspector"] as const) {
    const separator = separators[pane];
    if (!separator) continue;
    let dragging = false;
    let startX = 0;
    let startWidth = widths[pane];
    separator.addEventListener("pointerdown", (event) => {
      const e = event as PointerEvent;
      dragging = true;
      startX = e.clientX;
      startWidth = widths[pane];
      root.dataset.resizing = pane;
      separator.setPointerCapture?.(e.pointerId);
      e.preventDefault();
    });
    separator.addEventListener("pointermove", (event) => {
      if (!dragging) return;
      const e = event as PointerEvent;
      const delta = e.clientX - startX;
      applyWidth(pane, startWidth + (pane === "entities" ? delta : -delta), false);
      e.preventDefault();
    });
    const finish = (event: Event): void => {
      if (!dragging) return;
      dragging = false;
      delete root.dataset.resizing;
      saveWidth(pane);
      const e = event as PointerEvent;
      if (separator.hasPointerCapture?.(e.pointerId)) separator.releasePointerCapture(e.pointerId);
    };
    separator.addEventListener("pointerup", finish);
    separator.addEventListener("pointercancel", finish);
    separator.addEventListener("keydown", (event) => {
      let next: number | null = null;
      if (event.key === "Home") next = PANE_WIDTHS[pane].min;
      if (event.key === "End") next = PANE_WIDTHS[pane].max;
      if (event.key === "ArrowLeft") next = widths[pane] + (pane === "entities" ? -8 : 8);
      if (event.key === "ArrowRight") next = widths[pane] + (pane === "entities" ? 8 : -8);
      if (next === null) return;
      event.preventDefault();
      applyWidth(pane, next, true);
    });
  }

  const onKeyDown = (event: KeyboardEvent): void => {
    if (isEditableTarget(event.target)) return;
    if (
      (event.key === " " || event.key === "Spacebar") &&
      !event.repeat &&
      !event.altKey &&
      !event.ctrlKey &&
      !event.metaKey &&
      !event.shiftKey &&
      !nativeSpaceTarget(event.target)
    ) {
      if (setPeek(!peek)) event.preventDefault();
      return;
    }
    if (event.key !== "Escape" || event.altKey || event.ctrlKey || event.metaKey) return;
    let handled = false;
    if (peek) handled = setPeek(false);
    else if ((analysis?.get().comparison.length ?? 0) > 0) {
      analysis?.setComparison([]);
      handled = true;
    } else if ((mode === "stacked" || mode === "single") && level > 0) {
      handled = setLevel(level - 1);
    } else {
      const focus = analysis?.get().focus;
      if (focus?.highlighted) {
        analysis?.setFocus({ highlighted: null });
        handled = true;
      } else if (!((mode === "stacked" || mode === "single") && focus?.pane === "entities") && focus?.pane === "inspector") {
        analysis?.clearFocus();
        handled = true;
      }
    }
    if (handled) event.preventDefault();
  };

  const keyTarget: Document | HTMLElement = root.isConnected ? document : root;
  keyTarget.addEventListener("keydown", onKeyDown as EventListener);
  const onLayout = (event: Event): void => {
    const next = (event as CustomEvent<LayoutMode>).detail;
    if (next === "full" || next === "rail-icons" || next === "stacked" || next === "single") applyMode(next);
  };
  root.addEventListener("tare:layoutchange", onLayout);
  const onPaneLabel = (): void => sync();
  root.addEventListener("tare:pane-labelchange", onPaneLabel);

  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    keyTarget.removeEventListener("keydown", onKeyDown as EventListener);
    root.removeEventListener("tare:layoutchange", onLayout);
    root.removeEventListener("tare:pane-labelchange", onPaneLabel);
    root.removeEventListener("tare:dispose", dispose);
  };
  root.addEventListener("tare:dispose", dispose);
  applyMode(mode);
  return { dispose };
}
