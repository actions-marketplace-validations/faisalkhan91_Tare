// Adaptive layout state machine + scoped workspace status bar.

import { afterEach, describe, it, expect } from "vitest";
import {
  installAdaptivePaneInteractions,
  layoutMode,
  inspectorIsPeek,
  observeWorkbench,
  type LayoutMode,
} from "../src/shell/adaptivePanes.js";
import { createStatusBar } from "../src/shell/statusBar.js";
import { el } from "../src/ui/el.js";
import { createAnalysisStore, initialAnalysisState } from "../src/analysis/store.js";

function paneHarness(
  mode: LayoutMode,
  values = new Map<string, string>(),
  initialPane?: "entities" | "canvas" | "inspector"
) {
  const shell = el("div", { class: "shell", "data-layout": mode });
  const root = el("section", { "data-adaptive-panes": "" }, [
    el("nav", { "data-pane-nav": "" }, [
      el("button", { "data-pane-back": "", text: "Back" }),
      el("span", { "data-pane-title": "" }),
      el("button", { "data-pane-forward": "", text: "Forward" }),
    ]),
    el("div", { "data-pane": "entities" }),
    el("div", { "data-resize-pane": "entities" }),
    el("div", { "data-pane": "canvas" }),
    el("div", { "data-resize-pane": "inspector" }),
    el("div", { "data-pane": "inspector" }),
  ]);
  shell.appendChild(root);
  document.body.appendChild(shell);
  const analysis = createAnalysisStore(initialAnalysisState("investigate"));
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => void values.set(key, value),
  };
  const controller = installAdaptivePaneInteractions(root, { analysis, storage, initialPane });
  return { root, analysis, values, controller };
}

function key(target: EventTarget, value: string): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key: value, bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}

function pointer(target: EventTarget, type: string, clientX: number): void {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clientX", { value: clientX });
  Object.defineProperty(event, "pointerId", { value: 1 });
  target.dispatchEvent(event);
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("layoutMode at boundary widths", () => {
  it("maps each named width to the expected mode", () => {
    const cases: Array<[number, LayoutMode]> = [
      [330, "single"], // Windows Snap floor — still one usable pane
      [420, "single"], // desktop minimum
      [599, "single"],
      [600, "stacked"], // back/forward stack; rail behind a switcher
      [760, "stacked"],
      [899, "stacked"],
      [900, "rail-icons"], // rail collapses to icons; inspector = Peek
      [1100, "rail-icons"],
      [1279, "rail-icons"],
      [1280, "full"], // rail + entity + canvas + inspector
      [1440, "full"],
    ];
    for (const [px, mode] of cases) {
      expect(layoutMode(px), `${px}px`).toBe(mode);
    }
  });
  it("inspector is Peek only at medium widths; single mode gives it a normal stack level", () => {
    expect(inspectorIsPeek("full")).toBe(false);
    expect(inspectorIsPeek("rail-icons")).toBe(true);
    expect(inspectorIsPeek("stacked")).toBe(true);
    expect(inspectorIsPeek("single")).toBe(false);
  });
});

describe("observeWorkbench", () => {
  it("applies the initial mode from the container inline-size (no ResizeObserver in jsdom)", () => {
    const container = el("div");
    const seen: LayoutMode[] = [];
    const dispose = observeWorkbench(container, (m) => seen.push(m), () => 760);
    expect(seen).toEqual(["stacked"]);
    dispose();
  });
});

describe("adaptive pane interactions", () => {
  it("resizes both side panes by pointer and keyboard, clamps them, and persists the widths", () => {
    const first = paneHarness("full");
    const entity = first.root.querySelector<HTMLElement>('[data-resize-pane="entities"]')!;
    const inspector = first.root.querySelector<HTMLElement>('[data-resize-pane="inspector"]')!;

    pointer(entity, "pointerdown", 100);
    pointer(entity, "pointermove", 160); // 280 + 60, clamped at 320
    pointer(entity, "pointerup", 160);
    expect(entity.getAttribute("aria-valuenow")).toBe("320");
    expect(first.root.style.getPropertyValue("--entity-pane-w")).toBe("320px");

    key(inspector, "ArrowLeft"); // left edge moves left: inspector grows
    key(inspector, "End");
    expect(inspector.getAttribute("aria-valuenow")).toBe("340");
    expect(first.values.get("tare:pane-width:entities")).toBe("320");
    expect(first.values.get("tare:pane-width:inspector")).toBe("340");
    first.controller.dispose();
    first.root.closest(".shell")?.remove();

    const restored = paneHarness("full", first.values);
    expect(restored.root.style.getPropertyValue("--entity-pane-w")).toBe("320px");
    expect(restored.root.style.getPropertyValue("--inspector-pane-w")).toBe("340px");
    restored.controller.dispose();
  });

  it("toggles inspector Peek with Space only in the two middle modes and outside editable controls", () => {
    for (const mode of ["rail-icons", "stacked"] as const) {
      const h = paneHarness(mode);
      const inspector = h.root.querySelector<HTMLElement>('[data-pane="inspector"]')!;
      expect(inspector.hidden).toBe(true);
      expect(key(document.body, " ").defaultPrevented).toBe(true);
      expect(inspector.hidden).toBe(false);
      expect(h.root.dataset.inspectorPeek).toBe("true");

      const input = el("input");
      h.root.appendChild(input);
      key(input, " ");
      expect(inspector.hidden).toBe(false); // editable Space is untouched
      key(document.body, " ");
      expect(inspector.hidden).toBe(true);
      h.controller.dispose();
      h.root.closest(".shell")?.remove();
    }

    for (const mode of ["full", "single"] as const) {
      const h = paneHarness(mode);
      key(document.body, " ");
      expect(h.root.dataset.inspectorPeek).toBe("false");
      h.controller.dispose();
      h.root.closest(".shell")?.remove();
    }
  });

  it("provides level-naming Back/Forward navigation in stacked and single modes", () => {
    const stacked = paneHarness("stacked");
    const stackedTitle = stacked.root.querySelector('[data-pane-title]')!;
    expect(stackedTitle.textContent).toBe("Filters");
    (stacked.root.querySelector('[data-pane-forward]') as HTMLButtonElement).click();
    expect(stackedTitle.textContent).toBe("Results");
    expect(stacked.root.querySelector<HTMLElement>('[data-pane="entities"]')!.hidden).toBe(true);
    (stacked.root.querySelector('[data-pane-back]') as HTMLButtonElement).click();
    expect(stackedTitle.textContent).toBe("Filters");
    stacked.controller.dispose();
    stacked.root.closest(".shell")?.remove();

    const single = paneHarness("single");
    const next = single.root.querySelector('[data-pane-forward]') as HTMLButtonElement;
    next.click();
    next.click();
    expect(single.root.querySelector('[data-pane-title]')?.textContent).toBe("Explain");
    expect(single.root.querySelector<HTMLElement>('[data-pane="inspector"]')!.hidden).toBe(false);
    single.controller.dispose();
  });

  it("opens a deep-linked nested workspace on its requested pane without changing base defaults", () => {
    const selectedRun = paneHarness("stacked", new Map(), "canvas");
    expect(selectedRun.root.querySelector('[data-pane-title]')?.textContent).toBe("Results");
    expect(selectedRun.root.querySelector<HTMLElement>('[data-pane="canvas"]')!.hidden).toBe(false);
    expect(selectedRun.root.querySelector<HTMLElement>('[data-pane="entities"]')!.hidden).toBe(true);
    selectedRun.controller.dispose();
  });

  it("reveals a deep-linked facets inspector in every adaptive layout", () => {
    for (const mode of ["full", "rail-icons", "stacked", "single"] as const) {
      const h = paneHarness(mode, new Map(), "inspector");
      const inspector = h.root.querySelector<HTMLElement>('[data-pane="inspector"]')!;
      expect(inspector.hidden, `${mode} inspector`).toBe(false);
      if (mode === "rail-icons" || mode === "stacked") {
        expect(h.root.dataset.inspectorPeek).toBe("true");
        expect(h.analysis.get().focus.pane).toBe("inspector");
      }
      if (mode === "single") {
        expect(h.root.dataset.activePane).toBe("inspector");
        expect(h.root.querySelector('[data-pane-title]')?.textContent).toBe("Explain");
      }
      h.controller.dispose();
      h.root.closest(".shell")?.remove();
    }
  });

  it("unwinds one Esc layer at a time: Peek, comparison selection, then pane/drill focus", () => {
    const h = paneHarness("stacked");
    (h.root.querySelector('[data-pane-forward]') as HTMLButtonElement).click();
    h.analysis.setComparison([{ kind: "run", id: "r1" }]);
    key(document.body, " ");

    key(document.body, "Escape");
    expect(h.root.dataset.inspectorPeek).toBe("false");
    expect(h.analysis.get().comparison).toHaveLength(1);
    expect(h.root.querySelector('[data-pane-title]')?.textContent).toBe("Results");

    key(document.body, "Escape");
    expect(h.analysis.get().comparison).toEqual([]);
    expect(h.root.querySelector('[data-pane-title]')?.textContent).toBe("Results");

    key(document.body, "Escape");
    expect(h.root.querySelector('[data-pane-title]')?.textContent).toBe("Filters");
    expect(h.analysis.get().focus.pane).toBe("entities");
    h.controller.dispose();
  });
});

describe("scoped workspace status bar", () => {
  it("has the Workspace status label and no unrelated Today total", () => {
    const sb = createStatusBar(el);
    expect(sb.el.getAttribute("aria-label")).toBe("Workspace status");
    expect(sb.el.textContent).not.toContain("Today");
    expect(sb.el.querySelector(".tape-spend")).toBeNull();
  });
  it("reflects connection state without relying on color alone", () => {
    const sb = createStatusBar(el);
    sb.setConnection(true);
    expect(sb.el.querySelector(".conn-text")?.textContent).toBe("Local service connected");
    expect(sb.el.querySelector(".status-dot")?.getAttribute("aria-label")).toMatch(/connected/i);
    sb.setConnection(false);
    expect(sb.el.querySelector(".conn-text")?.textContent).toBe("Local service retrying");
  });
  it("shows the scoped cohort count + spend, and clears it (no leftover total)", () => {
    const sb = createStatusBar(el);
    sb.setScope({ count: 42, spendMicros: 3_500_000 });
    const scope = sb.el.querySelector(".status-scope")!;
    expect(scope.textContent).toContain("42 runs");
    expect(scope.textContent).toMatch(/\$/);
    sb.setScope(null);
    expect(scope.textContent).toBe("");
  });
  it("shows/hides a trust warning", () => {
    const sb = createStatusBar(el);
    const warn = sb.el.querySelector(".status-warning")!;
    expect(warn.hasAttribute("hidden")).toBe(true);
    sb.setWarning("aggregate-only");
    expect(warn.hasAttribute("hidden")).toBe(false);
    expect(warn.textContent).toBe("aggregate-only");
    sb.setWarning(null);
    expect(warn.hasAttribute("hidden")).toBe(true);
  });
});
