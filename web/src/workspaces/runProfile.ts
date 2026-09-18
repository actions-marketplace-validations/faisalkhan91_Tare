// Full Run Profile workspace. The canonical
// `#/investigate/run/:id` route is a bounded workbench instead of the legacy long Statement document:
// a sticky summary, an answer-first "Why this cost" section, routed Timeline/Profile/Shape/Provenance
// tabs, and a persistent run-context inspector for notes, exports, and recomputation attestation.
//
// Timing is evidence-gated: proxy and legacy rows remain Step order, while valid persisted
// timestamp spans get a BigInt elapsed view. One transient step selection cross-highlights the Beam,
// timeline, on-screen flamegraph, ranking table, and contextual inspector without changing exports.

import { el, rawSvg } from "../ui/el.js";
import { errorNode } from "../ui/errorNode.js";
import { emptyState } from "../ui/empty.js";
import { fmtDuration, fmtSignedUsd, fmtTokens, fmtUsd, humanizeKey } from "../ui/format.js";
import { receiptStatement } from "../ui/receiptStatement.js";
import { routePath } from "../ui/store.js";
import { statePill, runState } from "../ui/statePill.js";
import { tareBeam, type BeamGap, type BeamSegment } from "../ui/tareBeam.js";
import { renderSvgThemed, type FlameWeight, type FlamegraphModel } from "../svg.js";
import { cullSubPixel } from "../ui/flameView.js";
import { forgetRunReferences, recordOpenedRun } from "../ui/prefs.js";
import { installAdaptivePaneInteractions } from "../shell/adaptivePanes.js";
import {
  buildRunTimeline,
  formatTimelineNanos,
  relatedSteps,
  type RunTimelineModel,
} from "./runTimeline.js";
import {
  aggregatedProfile,
  applyFrameAction,
  chronologicalProfile,
  componentChoices,
  profileFrames,
  profileTableRows,
  sandwichProfile,
  type ProfileFlamegraphModel,
  type ProfileFrame,
  type ProfileFrameAction,
  type ProfileOrder,
  type ProfileTableMode,
} from "./runProfileModel.js";
import type {
  ProfileTable,
  RunMeta,
  RunNote,
  RunStatus,
  RunStep,
  SessionAutopsy,
  TareClient,
} from "../client.js";
import type { Route } from "../ui/store.js";
import type { WorkspaceContext } from "../shell/workbench.js";
import type { AnalysisState } from "../analysis/state.js";

export const RUN_PROFILE_TABS = ["timeline", "profile", "shape", "provenance"] as const;
export type RunProfileTab = (typeof RUN_PROFILE_TABS)[number];

const TAB_LABEL: Record<RunProfileTab, string> = {
  timeline: "Timeline",
  profile: "Profile",
  shape: "Shape",
  provenance: "Provenance",
};

function activeTab(route: Route): RunProfileTab {
  const view = route.query?.view;
  return (RUN_PROFILE_TABS as readonly string[]).includes(view ?? "")
    ? (view as RunProfileTab)
    : "profile";
}

type StepSelectionListener = (ordinal: number | null) => void;

interface StepSelection {
  get(): number | null;
  select(ordinal: number): void;
  subscribe(listener: StepSelectionListener): () => void;
  dispose(): void;
}

function stepEntityId(runId: string, ordinal: number): string {
  return `step:${encodeURIComponent(runId)}:${ordinal}`;
}

function focusStepForRun(context: WorkspaceContext | undefined, runId: string, steps: RunStep[]): number | null {
  const focus = context?.analysis.get().focus;
  const ordinal = focus?.stepOrdinal;
  return ordinal != null &&
    focus?.highlighted?.kind === "step" &&
    focus.highlighted.id === stepEntityId(runId, ordinal) &&
    steps.some((step) => step.ordinal === ordinal)
    ? ordinal
    : null;
}

/** Shared transient selection; AnalysisState carries it across routed tabs but never saves it. */
function createStepSelection(
  context: WorkspaceContext | undefined,
  runId: string,
  steps: RunStep[]
): StepSelection {
  const valid = new Set(steps.map((step) => step.ordinal));
  let current = focusStepForRun(context, runId, steps);
  const listeners = new Set<StepSelectionListener>();
  const emit = () => listeners.forEach((listener) => listener(current));
  const unsubscribe = context?.analysis.subscribe((state) => {
    const ordinal = state.focus.stepOrdinal;
    const next = ordinal != null &&
      state.focus.highlighted?.kind === "step" &&
      state.focus.highlighted.id === stepEntityId(runId, ordinal) &&
      valid.has(ordinal)
      ? ordinal
      : null;
    if (next === current) return;
    current = next;
    emit();
  });
  return {
    get: () => current,
    select(ordinal) {
      if (!valid.has(ordinal) || current === ordinal) return;
      if (context) {
        context.analysis.setFocus({
          pane: "canvas",
          highlighted: { kind: "step", id: stepEntityId(runId, ordinal), label: `Step ${ordinal}` },
          stepOrdinal: ordinal,
          framePath: [`step ${ordinal}`],
        });
      } else {
        current = ordinal;
        emit();
      }
    },
    subscribe(listener) {
      listeners.add(listener);
      listener(current);
      return () => listeners.delete(listener);
    },
    dispose() {
      unsubscribe?.();
      listeners.clear();
    },
  };
}

function withoutView(query: Record<string, string>): Record<string, string> {
  const { view: _view, ...scope } = query;
  return scope;
}

const RUN_ROW_HEIGHT = 42;
const RUN_ROW_OVERSCAN = 6;
const RUN_LIST_SCROLL_KEY = "tare:run-profile:list-scroll";
const RUN_LIST_FOCUS_KEY = "tare:run-profile:list-focus";

function sessionGet(key: string): string | null {
  try {
    return window.sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

function sessionSet(key: string, value: string): void {
  try {
    window.sessionStorage.setItem(key, value);
  } catch {
    // Pane position is ephemeral convenience state; a hardened browser can safely omit it.
  }
}

/**
 * Compact virtual run navigator for the bounded workbench. Only a screenful plus overscan exists in
 * the DOM even for a thousand captured runs; native links keep every run deep-linkable while the
 * keyboard loop and session scroll restoration make selection changes feel in-place.
 */
function runEntityPane(runIds: string[] | null, currentRun: string | null, route: Route): HTMLElement {
  const pane = el("aside", {
    id: "run-profile-entities",
    class: "run-profile-entities",
    "data-pane": "entities",
    "aria-label": "Captured runs",
  });
  pane.appendChild(el("div", { class: "run-profile-entities-head" }, [
    el("h2", { text: "Captured runs" }),
    el("p", { class: "caption sub", text: "Search locally by opaque run ID. Prompt text is never scanned." }),
  ]));
  if (runIds == null) {
    pane.appendChild(errorNode("Run list unavailable", "The open run remains available; retry to browse others."));
    return pane;
  }

  const ids = [...new Set(runIds.filter(Boolean))];
  if (currentRun && !ids.includes(currentRun)) ids.unshift(currentRun);
  const input = el("input", {
    class: "run-profile-run-search",
    type: "search",
    placeholder: "Filter run IDs…",
    "aria-label": "Filter captured run IDs",
  }) as HTMLInputElement;
  const count = el("p", { class: "run-profile-run-count caption sub", role: "status", "aria-live": "polite" });
  const viewport = el("nav", {
    class: "run-profile-run-scroll",
    "aria-label": "Run selection",
    tabindex: "0",
  });
  const list = el("div", { class: "run-profile-run-list", role: "list" });
  viewport.appendChild(list);
  pane.append(input, count, viewport);

  let filtered = ids;
  let keyboardIndex = Math.max(0, currentRun ? ids.indexOf(currentRun) : 0);
  const selectedQuery = { ...(route.query ?? {}) };

  const rememberPosition = (id: string): void => {
    sessionSet(RUN_LIST_SCROLL_KEY, String(Math.round(viewport.scrollTop)));
    sessionSet(RUN_LIST_FOCUS_KEY, id);
  };
  const focusLink = (id: string): void => {
    const link = Array.from(list.querySelectorAll<HTMLAnchorElement>("a[data-run-id]"))
      .find((candidate) => candidate.dataset.runId === id);
    link?.focus();
  };
  const draw = (): void => {
    const focusedRun = (document.activeElement as HTMLElement | null)?.dataset.runId;
    const height = viewport.clientHeight || RUN_ROW_HEIGHT * 10;
    const visibleRows = Math.max(1, Math.ceil(height / RUN_ROW_HEIGHT));
    const windowSize = visibleRows + RUN_ROW_OVERSCAN * 2;
    const start = Math.max(
      0,
      Math.min(
        Math.max(0, filtered.length - windowSize),
        Math.floor(viewport.scrollTop / RUN_ROW_HEIGHT) - RUN_ROW_OVERSCAN
      )
    );
    const end = Math.min(filtered.length, start + windowSize);
    list.style.height = `${filtered.length * RUN_ROW_HEIGHT}px`;
    const rows: HTMLElement[] = [];
    for (let index = start; index < end; index++) {
      const id = filtered[index];
      const active = id === currentRun;
      const link = el("a", {
        class: `run-profile-run-link${active ? " active" : ""}`,
        href: routePath(["investigate", "run", id], selectedQuery),
        "data-run-id": id,
        title: id,
        "aria-current": active ? "page" : undefined,
      }, [
        el("span", { class: "run-profile-run-id", text: id }),
        active ? el("span", { class: "caption", text: "Open" }) : null,
      ]) as HTMLAnchorElement;
      link.addEventListener("focus", () => {
        keyboardIndex = index;
      });
      link.addEventListener("click", () => rememberPosition(id));
      rows.push(el("div", {
        class: "run-profile-run-row",
        role: "listitem",
        style: `top:${index * RUN_ROW_HEIGHT}px;height:${RUN_ROW_HEIGHT}px`,
      }, [link]));
    }
    list.replaceChildren(...rows);
    // A browser scroll event can arrive after the route-restoration microtask. Re-windowing must not
    // detach focus and strand it on <body>; restore the same run link after replacing virtual rows.
    if (focusedRun) queueMicrotask(() => focusLink(focusedRun));
    count.textContent = `${filtered.length} of ${ids.length} run${ids.length === 1 ? "" : "s"}`;
  };

  input.addEventListener("input", () => {
    const query = input.value.trim().toLowerCase();
    filtered = query ? ids.filter((id) => id.toLowerCase().includes(query)) : ids;
    keyboardIndex = Math.max(0, currentRun ? filtered.indexOf(currentRun) : 0);
    viewport.scrollTop = 0;
    draw();
  });
  viewport.addEventListener("scroll", draw);
  viewport.addEventListener("keydown", (event) => {
    const activeId = (document.activeElement as HTMLElement | null)?.dataset.runId;
    const activeIndex = activeId ? filtered.indexOf(activeId) : keyboardIndex;
    let next = -1;
    if (event.key === "ArrowDown" || event.key === "j") next = Math.min(filtered.length - 1, activeIndex + 1);
    else if (event.key === "ArrowUp" || event.key === "k") next = Math.max(0, activeIndex - 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = filtered.length - 1;
    if (next < 0 || filtered.length === 0) return;
    event.preventDefault();
    keyboardIndex = next;
    const top = next * RUN_ROW_HEIGHT;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (top + RUN_ROW_HEIGHT > viewport.scrollTop + (viewport.clientHeight || RUN_ROW_HEIGHT * 10)) {
      viewport.scrollTop = top - (viewport.clientHeight || RUN_ROW_HEIGHT * 10) + RUN_ROW_HEIGHT;
    }
    draw();
    queueMicrotask(() => focusLink(filtered[next]));
  });

  /// Run `fn` once the pane is attached and measurable. A microtask is too early — the caller appends
  /// this pane after we return — and jsdom has no rAF in some harnesses, hence the fallback.
  const scheduleAfterLayout = (fn: () => void): void => {
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(fn);
    else queueMicrotask(fn);
  };
  const restoreListFocus = (): void => {
    const target = sessionGet(RUN_LIST_FOCUS_KEY);
    if (!target || target !== currentRun) return;
    focusLink(target);
    sessionSet(RUN_LIST_FOCUS_KEY, "");
  };

  const savedScroll = Number(sessionGet(RUN_LIST_SCROLL_KEY));
  draw();
  if (Number.isFinite(savedScroll) && savedScroll > 0) {
    // Restore AFTER layout, not in a microtask. This pane is still detached when the microtask runs
    // (the caller appends it after we return), so `viewport.clientHeight` was 0 → `draw()` rendered a
    // one-row window → the spacer was shorter than `savedScroll` → the browser CLAMPED scrollTop to
    // 0, and the list snapped to the top on every run-to-run navigation, losing the user's place.
    // A frame later the element is attached and measurable, so the assignment sticks; the second
    // `draw()` then fills the newly-visible window (covered by the runs-workbench e2e suite).
    scheduleAfterLayout(() => {
      draw(); // re-measure now that clientHeight is real, so the spacer can hold savedScroll
      viewport.scrollTop = savedScroll;
      draw(); // render the rows the restored offset exposes
      // Focus AFTER the scroll restore, in the same frame: the row is only in the virtualized window
      // once the offset is applied, so focusing first would either miss the element entirely or
      // scroll it into view and undo the restore.
      restoreListFocus();
    });
  } else {
    scheduleAfterLayout(restoreListFocus);
  }
  return pane;
}

function paneNavigation(): HTMLElement {
  return el("nav", {
    class: "run-profile-pane-nav",
    "data-pane-nav": "",
    "aria-label": "Run Profile pane navigation",
    hidden: true,
  }, [
    el("button", { class: "btn", type: "button", "data-pane-back": "", text: "Back" }),
    el("span", { class: "run-profile-pane-title", "data-pane-title": "", "aria-live": "polite" }),
    el("button", { class: "btn", type: "button", "data-pane-forward": "", text: "Forward" }),
  ]);
}

function paneResizer(pane: "entities" | "inspector", controls: string): HTMLElement {
  return el("div", {
    class: "run-profile-resizer",
    "data-resize-pane": pane,
    "aria-controls": controls,
  });
}

function mountRunWorkbench(
  root: HTMLElement,
  header: HTMLElement,
  entities: HTMLElement,
  canvas: HTMLElement,
  inspector: HTMLElement,
  context: WorkspaceContext | undefined,
  initialPane: "entities" | "canvas",
  selection?: StepSelection
): void {
  const workbench = el("article", {
    class: "run-profile",
    "data-adaptive-panes": "",
    "aria-label": "Run Profile workbench",
  }, [
    header,
    paneNavigation(),
    el("div", { class: "run-profile-body" }, [
      entities,
      paneResizer("entities", "run-profile-entities"),
      canvas,
      paneResizer("inspector", "run-profile-inspector"),
      inspector,
    ]),
  ]);
  root.replaceChildren(workbench);
  const main = root.closest<HTMLElement>(".main");
  main?.classList.add("main-bounded-workbench");
  workbench.addEventListener("tare:dispose", () => {
    main?.classList.remove("main-bounded-workbench");
    selection?.dispose();
  }, { once: true });
  installAdaptivePaneInteractions(workbench, { analysis: context?.analysis, initialPane });
}

function observedDuration(steps: RunStep[]): string {
  let start: bigint | null = null;
  let end: bigint | null = null;
  for (const step of steps) {
    if (!step.start_unix_nano || !step.end_unix_nano) continue;
    try {
      const s = BigInt(step.start_unix_nano);
      const e = BigInt(step.end_unix_nano);
      start = start == null || s < start ? s : start;
      end = end == null || e > end ? e : end;
    } catch {
      // Invalid timing is a capture gap. Never guess a duration from malformed metadata.
    }
  }
  if (start == null || end == null || end < start) return "Not captured";
  const ms = Number((end - start) / 1_000_000n);
  return Number.isFinite(ms) ? fmtDuration(ms) : "Not captured";
}

function summaryDatum(label: string, value: Node | string, detail?: string): HTMLElement {
  return el("div", { class: "run-summary-datum" }, [
    el("dt", { text: label }),
    el("dd", {}, [
      value,
      detail ? el("span", { class: "caption sub", text: detail }) : null,
    ]),
  ]);
}

function baselineSummary(
  state: AnalysisState | undefined,
  status: RunStatus,
  baseline: { data: { total_micros: number; run_count: number } } | null
): { value: string; detail: string } {
  if (!state?.baseline) return { value: "Not selected", detail: "Choose Baseline B in Investigate" };
  if (status.unpriced) return { value: "Unavailable", detail: `${state.baseline.label} · run has unpriced usage` };
  if (!baseline || baseline.data.run_count < 1) {
    return { value: "Unavailable", detail: `${state.baseline.label} · no comparable runs` };
  }
  const average = Math.round(baseline.data.total_micros / baseline.data.run_count);
  return {
    value: fmtSignedUsd(status.micros - average),
    detail: `${state.baseline.label} average · n=${baseline.data.run_count}`,
  };
}

function stickySummary(
  status: RunStatus,
  meta: RunMeta | null,
  steps: RunStep[],
  autopsy: SessionAutopsy | null,
  quality: number | undefined,
  baseline: { value: string; detail: string }
): HTMLElement {
  const spend = status.unpriced && status.micros === 0 ? "Unpriced" : fmtUsd(status.micros);
  const spendDetail = status.unpriced
    ? status.micros > 0
      ? "Priced portion only · additional usage is unpriced"
      : "No price found · never treated as $0"
    : "Estimated from captured priced tokens";
  const state = runState(status);
  return el("dl", { class: "run-profile-summary", "aria-label": "Run summary", tabindex: "0" }, [
    summaryDatum("Est. priced spend", spend, spendDetail),
    summaryDatum("Baseline delta", baseline.value, baseline.detail),
    summaryDatum("Steps", fmtTokens(status.steps)),
    summaryDatum("Duration", observedDuration(steps), "Observed timestamp span"),
    summaryDatum("Quality", quality == null ? "Not supplied" : String(quality), "User supplied"),
    summaryDatum("State", statePill(state)),
    summaryDatum("Model", meta?.models.join(", ") || status.last_model || "Not recorded"),
    summaryDatum("Fidelity", autopsy?.fidelity ? humanizeKey(autopsy.fidelity) : "Not recorded"),
  ]);
}

function whyHeadline(autopsy: SessionAutopsy | null, status: RunStatus): HTMLElement {
  if (!autopsy) {
    return el("p", {
      class: "run-profile-why-headline",
      text: status.top_cause
        ? `${humanizeKey(status.top_cause)} is the largest recorded cost cause.`
        : "No cost-cause explanation was available for this run.",
    });
  }
  switch (autopsy.headline.kind) {
    case "waste":
      return el("p", { class: "run-profile-why-headline" }, [
        el("strong", { text: autopsy.headline.detail.label }),
        ` · ${fmtUsd(autopsy.headline.detail.recoverable_micros)} capped potential`,
      ]);
    case "structural_driver":
      return el("p", { class: "run-profile-why-headline" }, [
        el("strong", { text: `${humanizeKey(autopsy.headline.detail.class)} is the largest structural driver.` }),
        ` ${fmtUsd(autopsy.headline.detail.micros)} measured; ${autopsy.headline.detail.vs_median_pct}% of median.`,
      ]);
    case "efficient":
      return el("p", {
        class: "run-profile-why-headline",
        text: "No measured waste opportunity was detected; the recorded cost is structural for this run.",
      });
  }
}

function stepBeamValue(step: RunStep, key: string): number {
  switch (key) {
    case "fresh_input": return step.fresh_input;
    case "cache_read": return step.cache_read;
    case "cache_write": return step.cache_write;
    case "output": return step.output;
    case "reasoning": return step.reasoning;
    case "priced": return step.micros;
    default: return 0;
  }
}

function runBeam(
  status: RunStatus,
  steps: RunStep[],
  autopsy: SessionAutopsy | null,
  selection: StepSelection
): HTMLElement {
  const tone: Record<string, string> = {
    fresh_input: "cat-1",
    cache_read: "cat-2",
    cache_write: "cat-3",
    output: "cat-4",
    reasoning: "cat-5",
  };
  const segments: BeamSegment[] = (autopsy?.classes ?? [])
    .filter((row) => row.micros > 0)
    .map((row) => ({
      key: row.class,
      label: humanizeKey(row.class),
      value: row.micros,
      tone: tone[row.class],
    }));
  const unpricedTokens = steps
    .filter((step) => step.tokens > 0 && step.micros === 0)
    .reduce((sum, step) => sum + step.tokens, 0);
  const totalTokens = steps.reduce((sum, step) => sum + step.tokens, 0);
  const gap: BeamGap | undefined =
    unpricedTokens > 0
      ? {
          label: "Unpriced usage",
          tokens: unpricedTokens,
          share: totalTokens > 0 ? unpricedTokens / totalTokens : undefined,
        }
      : undefined;
  // If the run has priced spend but the autopsy is unavailable, keep one honest aggregate segment.
  if (segments.length === 0 && status.micros > 0) {
    segments.push({ key: "priced", label: "Captured priced spend", value: status.micros, tone: "cat-1" });
  }
  const candidateByKey = new Map<string, RunStep>();
  for (const segment of segments) {
    const candidate = [...steps]
      .filter((step) => stepBeamValue(step, segment.key) > 0)
      .sort((a, b) => stepBeamValue(b, segment.key) - stepBeamValue(a, segment.key) || a.ordinal - b.ordinal)[0];
    if (candidate) candidateByKey.set(segment.key, candidate);
  }
  const beam = tareBeam({
    mode: "run",
    title: "Run cost composition",
    unit: "usd",
    segments,
    gap,
    onSelect: candidateByKey.size > 0
      ? (key) => {
          const candidate = candidateByKey.get(key);
          if (candidate) selection.select(candidate.ordinal);
        }
      : undefined,
  });
  const selectionStatus = el("p", {
    class: "caption sub run-profile-beam-selection",
    role: "status",
    "aria-live": "polite",
  });
  beam.appendChild(selectionStatus);
  for (const button of Array.from(beam.querySelectorAll<HTMLButtonElement>("button[data-beam-key]"))) {
    const key = button.dataset.beamKey ?? "";
    const candidate = candidateByKey.get(key);
    if (candidate) button.title = `Select Step ${candidate.ordinal}, the largest captured ${button.textContent?.split(" · ")[0] ?? key} contributor`;
  }
  selection.subscribe((ordinal) => {
    const step = steps.find((row) => row.ordinal === ordinal);
    beam.classList.toggle("has-step-selection", Boolean(step));
    const keys = new Set(
      step ? segments.filter((segment) => stepBeamValue(step, segment.key) > 0).map((segment) => segment.key) : []
    );
    for (const node of Array.from(beam.querySelectorAll<HTMLElement>("[data-beam-key]"))) {
      const on = keys.has(node.dataset.beamKey ?? "");
      node.classList.toggle("is-cross-highlighted", on);
      node.toggleAttribute("data-cross-highlighted", on);
    }
    const labels = segments.filter((segment) => keys.has(segment.key)).map((segment) => segment.label);
    selectionStatus.textContent = step
      ? `Step ${step.ordinal} includes ${labels.length ? labels.join(", ") : "no priced composition segment"}.`
      : "Select a step to cross-highlight its captured composition.";
  });
  return beam;
}

function whySection(
  status: RunStatus,
  steps: RunStep[],
  autopsy: SessionAutopsy | null,
  selection: StepSelection
): HTMLElement {
  const top = autopsy?.opportunities[0];
  return el("section", { class: "run-profile-why", "aria-labelledby": "run-profile-why-title" }, [
    el("div", { class: "run-profile-section-head" }, [
      el("h2", { id: "run-profile-why-title", text: "Why this cost" }),
      top
        ? el("span", {
            class: "caption",
            text: `Top ${humanizeKey(top.confidence).toLowerCase()} opportunity · ${fmtUsd(top.recoverable_micros)}`,
          })
        : null,
    ]),
    whyHeadline(autopsy, status),
    runBeam(status, steps, autopsy, selection),
    top
      ? el("p", { class: "run-profile-opportunity" }, [
          el("strong", { text: "Action: " }),
          top.fix_text,
          ` · effort ${top.effort}`,
        ])
      : null,
  ]);
}

function tabBar(route: Route, active: RunProfileTab): HTMLElement {
  const tabs = el("nav", {
    class: "run-profile-tabs",
    role: "tablist",
    "aria-label": "Run Profile views",
  });
  const links: HTMLAnchorElement[] = [];
  for (const tab of RUN_PROFILE_TABS) {
    const link = el("a", {
      id: `run-profile-tab-${tab}`,
      class: `run-profile-tab${tab === active ? " active" : ""}`,
      role: "tab",
      href: routePath(route.segments, { ...(route.query ?? {}), view: tab }),
      "aria-selected": tab === active ? "true" : "false",
      "aria-controls": "run-profile-panel",
      tabindex: tab === active ? "0" : "-1",
      text: TAB_LABEL[tab],
    }) as HTMLAnchorElement;
    links.push(link);
    tabs.appendChild(link);
  }
  // Roving focus follows the WAI-ARIA automatic-activation tab pattern. Links keep each view
  // deep-linkable and make keyboard activation use the same scope-preserving route as a click.
  tabs.addEventListener("keydown", (event) => {
    const selected = links.findIndex((link) => link.getAttribute("aria-selected") === "true");
    const focused = links.indexOf(document.activeElement as HTMLAnchorElement);
    const current = focused >= 0 ? focused : selected;
    const last = links.length - 1;
    let next = -1;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") next = current >= last ? 0 : current + 1;
    else if (event.key === "ArrowLeft" || event.key === "ArrowUp") next = current <= 0 ? last : current - 1;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    if (next >= 0) {
      event.preventDefault();
      links[next].focus();
      links[next].click();
    }
  });
  return tabs;
}

const STEP_WINDOW_SIZE = 100;

function timelineEvidenceCopy(model: RunTimelineModel): string {
  if (model.concurrentCount > 0) {
    return `${model.concurrentCount} overlapping sibling span pair${model.concurrentCount === 1 ? "" : "s"} has timestamp, trace, and shared-parent evidence; concurrent execution is confirmed only for those pairs.`;
  }
  if (model.overlapCount > 0) {
    return `${model.overlapCount} timestamp overlap${model.overlapCount === 1 ? "" : "s"} is visible, but no overlapping sibling-span relationship was captured; concurrency is not claimed.`;
  }
  return "No timed spans overlap; concurrency is not claimed.";
}

function stepFacts(step: RunStep): Node[] {
  return [
    el("span", { class: "num", text: `#${step.ordinal}` }),
    el("span", { text: step.model || step.provider || "Unknown model" }),
    el("span", { class: "num", text: step.micros === 0 && step.tokens > 0 ? "Unpriced" : fmtUsd(step.micros) }),
    el("span", { class: "num sub", text: `${fmtTokens(step.tokens)} tokens` }),
  ];
}

function timelinePanel(model: RunTimelineModel, selection: StepSelection): HTMLElement {
  const trueTimeline = model.mode === "timeline";
  const panel = el("section", { class: "run-profile-tab-content run-profile-timeline" }, [
    el("h2", { text: trueTimeline ? "Timeline" : "Step order" }),
    el("p", {
      class: "caption",
      text: trueTimeline
        ? `${model.timedCount} of ${model.totalCount} steps carry valid timestamp spans. Positions are elapsed from the first observed timestamp; untimed rows remain order-only.`
        : "Captured order only. Local latency may be shown per step, but no elapsed position or concurrency is implied without real timestamps.",
    }),
    trueTimeline
      ? el("p", {
          class: `run-profile-timeline-evidence ${model.concurrentCount ? "trust-ok" : "sub"}`,
          "data-concurrency-evidence": model.concurrentCount ? "confirmed" : "not-claimed",
          text: timelineEvidenceCopy(model),
        })
      : null,
  ]);
  if (model.totalCount === 0) {
    panel.appendChild(emptyState("No steps captured", "This run has no step-level records to order."));
    return panel;
  }

  if (trueTimeline) {
    panel.appendChild(el("div", { class: "run-profile-timeline-axis", "aria-hidden": "true" }, [
      el("span", { text: "0 ms" }),
      el("span", { text: formatTimelineNanos(model.duration / 2n) }),
      el("span", { text: formatTimelineNanos(model.duration) }),
    ]));
  }

  const ordered = model.orderedSteps;
  let selected = selection.get();
  const selectedIndex = selected == null ? -1 : ordered.findIndex((step) => step.ordinal === selected);
  let windowStart = selectedIndex >= STEP_WINDOW_SIZE
    ? Math.max(0, selectedIndex - Math.floor(STEP_WINDOW_SIZE / 2))
    : 0;
  const list = el("ol", { class: "run-profile-step-list", "aria-label": trueTimeline ? "Timed and order-only steps" : "Captured step order" });
  panel.appendChild(list);
  const paging = el("div", { class: "run-profile-more" });
  panel.appendChild(paging);

  const updateSelection = (): void => {
    for (const row of Array.from(list.querySelectorAll<HTMLElement>("[data-step-ordinal]"))) {
      const on = Number(row.dataset.stepOrdinal) === selected;
      row.classList.toggle("is-cross-highlighted", on);
      row.toggleAttribute("data-cross-highlighted", on);
      row.querySelector("button")?.setAttribute("aria-pressed", String(on));
    }
  };
  const focusSelected = (): void => {
    list.querySelector<HTMLButtonElement>(`[data-step-ordinal="${selected}"] button`)?.focus();
  };
  const draw = (): void => {
    const windowEnd = Math.min(ordered.length, windowStart + STEP_WINDOW_SIZE);
    const rows = ordered.slice(windowStart, windowEnd).map((step) => {
      const timed = model.timedByOrdinal.get(step.ordinal);
      const button = el("button", {
        class: "run-profile-step-select",
        type: "button",
        "aria-pressed": "false",
        onClick: () => selection.select(step.ordinal),
      }, [
        ...stepFacts(step),
        timed
          ? el("span", { class: "run-profile-step-time num" }, [
              el("span", { text: `+${formatTimelineNanos(timed.offset)}` }),
              el("span", { class: "sub", text: ` for ${formatTimelineNanos(timed.duration)}` }),
            ])
          : step.duration_ms && step.duration_ms > 0
            ? el("span", { class: "num sub", text: `${fmtDuration(step.duration_ms)} local latency`, title: "Observed local step latency; not a timeline position" })
            : el("span", { class: "sub", text: trueTimeline ? "Timestamp not captured · order only" : "Latency not captured" }),
      ]) as HTMLButtonElement;
      button.addEventListener("keydown", (event) => {
        if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
        event.preventDefault();
        const index = ordered.findIndex((row) => row.ordinal === step.ordinal);
        const next = event.key === "ArrowDown" ? Math.min(ordered.length - 1, index + 1) : Math.max(0, index - 1);
        selection.select(ordered[next].ordinal);
        queueMicrotask(focusSelected);
      });
      return el("li", {
        class: `run-profile-step${timed ? " is-timed" : " is-order-only"}`,
        "data-step-ordinal": String(step.ordinal),
        "data-timed": String(Boolean(timed)),
      }, [
        button,
        timed
          ? el("div", { class: "run-profile-timeline-track", "aria-hidden": "true" }, [
              el("span", {
                class: "run-profile-timeline-bar",
                style: `left:${timed.leftPct.toFixed(2)}%;width:max(3px,${timed.widthPct.toFixed(2)}%)`,
              }),
            ])
          : null,
      ]);
    });
    list.replaceChildren(...rows);
    paging.replaceChildren(
      el("button", {
        class: "btn",
        type: "button",
        disabled: windowStart === 0 ? "" : undefined,
        text: "Previous 100",
        onClick: () => {
          windowStart = Math.max(0, windowStart - STEP_WINDOW_SIZE);
          draw();
        },
      }),
      el("span", { class: "caption sub", text: `Showing ${windowStart + 1}–${windowEnd} of ${ordered.length}` }),
      el("button", {
        class: "btn",
        type: "button",
        disabled: windowEnd >= ordered.length ? "" : undefined,
        text: "Next 100",
        onClick: () => {
          windowStart = Math.min(Math.max(0, ordered.length - STEP_WINDOW_SIZE), windowStart + STEP_WINDOW_SIZE);
          draw();
        },
      })
    );
    updateSelection();
  };
  draw();
  selection.subscribe((ordinal) => {
    selected = ordinal;
    const index = ordinal == null ? -1 : ordered.findIndex((step) => step.ordinal === ordinal);
    if (index >= 0 && (index < windowStart || index >= windowStart + STEP_WINDOW_SIZE)) {
      windowStart = Math.max(0, Math.min(ordered.length - STEP_WINDOW_SIZE, index - Math.floor(STEP_WINDOW_SIZE / 2)));
      draw();
    } else {
      updateSelection();
    }
  });
  return panel;
}

function fallbackProfileTable(profile: ProfileTable): HTMLElement {
  return el("table", { class: "data run-profile-cost-table" }, [
    el("thead", {}, [
      el("tr", {}, [
        el("th", { text: "Frame" }),
        el("th", { class: "num", text: "Self cost" }),
        el("th", { class: "num", text: "Cumulative cost" }),
        el("th", { text: "Calls" }),
        el("th", { text: "Cost / call" }),
      ]),
    ]),
    el("tbody", {}, profile.rows.slice(0, 100).map((row) => el("tr", {}, [
      el("td", { text: row.name }),
      el("td", { class: "num", text: fmtUsd(row.self_micros) }),
      el("td", { class: "num", text: fmtUsd(row.cum_micros) }),
      el("td", { class: "sub", text: "Not recorded" }),
      el("td", { class: "sub", text: "Not recorded" }),
    ]))),
  ]);
}

function profilePanel(
  flame: FlamegraphModel | null,
  profile: ProfileTable | null,
  selection: StepSelection
): HTMLElement {
  const panel = el("section", { class: "run-profile-tab-content" }, [
    el("h2", { text: "Cost and token profile" }),
  ]);
  if (!flame) {
    panel.appendChild(errorNode("Cost profile unavailable", "No flamegraph was returned"));
    if (profile?.rows.length) panel.appendChild(fallbackProfileTable(profile));
    return panel;
  }

  // With no priced dollars there is no cost geometry to profile. Show the captured token structure
  // immediately; any priced or partially-priced run still starts in the canonical Cost mode.
  let weight: FlameWeight = flame.root.micros <= 0 && flame.root.tokens > 0 ? "tokens" : "cost";
  let order: ProfileOrder = "chronological";
  let tableMode: ProfileTableMode = "cumulative";
  let query = "";
  let componentKey = "";
  let frameTargetKey = "";
  let frameAction: { action: ProfileFrameAction; frameKey: string } | null = null;
  let selectedFrameKey: string | null = null;
  let selectedOrdinal = selection.get();

  const caption = el("p", { class: "caption run-profile-profile-caption" });
  const status = el("p", {
    class: "caption sub run-profile-profile-status",
    role: "status",
    "aria-live": "polite",
  });
  const flameHost = el("div", { class: "run-profile-flame", role: "group" });
  const tableHost = el("div", { class: "run-profile-profile-table-wrap" });

  const weightButtons = (["cost", "tokens"] as const).map((value) =>
    el("button", {
      class: "btn",
      type: "button",
      "data-profile-weight": value,
      text: value === "cost" ? "Cost" : "Tokens",
    }) as HTMLButtonElement
  );
  const orderButtons = (["chronological", "aggregated", "sandwich"] as const).map((value) =>
    el("button", {
      class: "btn",
      type: "button",
      "data-profile-order": value,
      text: value[0].toUpperCase() + value.slice(1),
    }) as HTMLButtonElement
  );
  const tableButtons = (["flat", "cumulative"] as const).map((value) =>
    el("button", {
      class: "btn",
      type: "button",
      "data-profile-table": value,
      text: value === "flat" ? "Flat (self)" : "Cumulative",
    }) as HTMLButtonElement
  );
  const componentSelect = el("select", {
    class: "run-profile-component-select",
    "aria-label": "Selected component for Sandwich",
  }) as HTMLSelectElement;
  const search = el("input", {
    class: "run-profile-frame-search",
    type: "search",
    placeholder: "Search frames…",
    "aria-label": "Search profile frames",
  }) as HTMLInputElement;
  const frameSelect = el("select", {
    class: "run-profile-frame-target",
    "aria-label": "Frame for profiler action",
  }) as HTMLSelectElement;
  const actionButtons = (["focus", "ignore", "hide"] as const).map((action) =>
    el("button", {
      class: "btn",
      type: "button",
      "data-profile-action": action,
      text: action[0].toUpperCase() + action.slice(1),
    }) as HTMLButtonElement
  );
  const clearAction = el("button", {
    class: "btn ghost",
    type: "button",
    text: "Clear",
  }) as HTMLButtonElement;

  const controls = el("div", { class: "run-profile-profile-controls", "aria-label": "Profile controls" }, [
    el("fieldset", {}, [el("legend", { text: "Weight" }), ...weightButtons]),
    el("fieldset", {}, [el("legend", { text: "Ordering" }), ...orderButtons]),
    el("label", {}, [el("span", { text: "Sandwich component" }), componentSelect]),
    el("fieldset", {}, [el("legend", { text: "Table ranking" }), ...tableButtons]),
    el("label", {}, [el("span", { text: "Search" }), search]),
    el("label", {}, [el("span", { text: "Frame" }), frameSelect]),
    el("fieldset", { class: "run-profile-frame-actions" }, [
      el("legend", { text: "Profiler action" }),
      ...actionButtons,
      clearAction,
    ]),
  ]);
  panel.append(caption, controls, status, flameHost, tableHost);

  const buildBase = (): ProfileFlamegraphModel | null => {
    if (order === "chronological") return chronologicalProfile(flame);
    if (order === "aggregated") return aggregatedProfile(flame, weight);
    return componentKey ? sandwichProfile(flame, componentKey, weight) : null;
  };

  const ordinalsFor = (frame: ProfileFrame): number[] =>
    frame.references
      .filter((ref) => ref.run_id === flame.run_id)
      .map((ref) => ref.step_ordinal)
      .filter((value, index, all) => all.indexOf(value) === index)
      .sort((a, b) => a - b);

  const updateCrossHighlights = (): void => {
    flameHost.classList.toggle("has-step-selection", selectedOrdinal != null);
    for (const frameNode of Array.from(flameHost.querySelectorAll<SVGElement>("[data-step-ordinals]"))) {
      const ordinals = (frameNode.dataset.stepOrdinals ?? "").split(" ").map(Number);
      const on = selectedOrdinal != null && ordinals.includes(selectedOrdinal);
      frameNode.classList.toggle("is-cross-highlighted", on);
      frameNode.classList.toggle("is-cross-muted", selectedOrdinal != null && !on);
      frameNode.setAttribute("aria-pressed", String(frameNode.dataset.frameKey === selectedFrameKey || on));
    }
    for (const row of Array.from(tableHost.querySelectorAll<HTMLElement>("tr[data-step-ordinals]"))) {
      const ordinals = (row.dataset.stepOrdinals ?? "").split(" ").map(Number);
      const on = selectedOrdinal != null && ordinals.includes(selectedOrdinal);
      row.classList.toggle("is-cross-highlighted", on);
      row.toggleAttribute("data-cross-highlighted", on);
    }
  };

  const chooseFrame = (frame: ProfileFrame): void => {
    selectedFrameKey = frame.frame_key;
    frameTargetKey = frame.frame_key;
    frameSelect.value = frame.frame_key;
    if (frame.node_kind === "component") {
      componentKey = frame.frame_key;
      componentSelect.value = componentKey;
    }
    const ordinals = ordinalsFor(frame);
    if (ordinals.length === 1) selection.select(ordinals[0]);
    status.textContent = ordinals.length > 1
      ? `${frame.display_label}: ${frame.calls} calls across ${ordinals.length} contributing steps.`
      : `${frame.display_label}: ${frame.calls} call${frame.calls === 1 ? "" : "s"}.`;
    for (const node of Array.from(flameHost.querySelectorAll<SVGElement>("[data-frame-key]"))) {
      node.setAttribute("aria-pressed", String(node.dataset.frameKey === selectedFrameKey));
    }
  };

  const paint = (): void => {
    weightButtons.forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.profileWeight === weight)));
    orderButtons.forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.profileOrder === order)));
    tableButtons.forEach((button) => button.setAttribute("aria-pressed", String(button.dataset.profileTable === tableMode)));
    actionButtons.forEach((button) => {
      button.disabled = !frameTargetKey;
      button.setAttribute("aria-pressed", String(frameAction?.action === button.dataset.profileAction));
    });
    clearAction.disabled = frameAction == null;
    caption.textContent = `On-screen width and ranking use ${weight === "cost" ? "estimated cost" : "captured tokens"}. ${order === "chronological" ? "Captured step/component order is preserved." : order === "aggregated" ? "Equal FrameKeys merge recursively within their full caller path." : "Sandwich shows callers, the selected component, and its callees."} Export SVG remains byte-stable and token-weighted.`;

    const choices = componentChoices(flame, weight);
    componentSelect.replaceChildren(
      el("option", { value: "", text: "Select a component…" }),
      ...choices.map((choice) => el("option", {
        value: choice.frame_key,
        text: `${choice.label} · ${choice.calls} call${choice.calls === 1 ? "" : "s"}`,
      }))
    );
    componentSelect.value = componentKey;

    const base = buildBase();
    if (!base) {
      flameHost.replaceChildren(emptyState("Choose a Sandwich component", "Select one recorded component to see its callers and callees."));
      tableHost.replaceChildren();
      status.textContent = "Sandwich requires an explicit component selection.";
      return;
    }

    const frameOptions = new Map<string, ProfileFrame>();
    for (const frame of profileFrames(base).slice(1)) {
      if (!frameOptions.has(frame.frame_key)) frameOptions.set(frame.frame_key, frame);
    }
    const sortedOptions = [...frameOptions.values()].sort((a, b) =>
      a.node_kind.localeCompare(b.node_kind) || a.display_label.localeCompare(b.display_label)
    );
    frameSelect.replaceChildren(
      el("option", { value: "", text: "Choose a frame…" }),
      ...sortedOptions.map((frame) => el("option", {
        value: frame.frame_key,
        text: `${humanizeKey(frame.node_kind)} · ${frame.display_label}`,
      }))
    );
    if (!frameOptions.has(frameTargetKey)) {
      frameTargetKey = "";
      frameAction = null;
    }
    frameSelect.value = frameTargetKey;

    const transformed = frameAction
      ? applyFrameAction(base, frameAction.action, frameAction.frameKey)
      : base;
    const displayed = cullSubPixel(transformed, 1, 960, weight) as ProfileFlamegraphModel;
    const indexed = profileFrames(displayed);
    flameHost.setAttribute(
      "aria-label",
      `Interactive ${order} run profile; frame width is ${weight === "cost" ? "estimated cost" : "token"} share`
    );
    rawSvg(flameHost, renderSvgThemed(displayed, weight));
    const normalizedQuery = query.trim().toLowerCase();
    Array.from(flameHost.querySelectorAll<SVGElement>("[data-frame]")).forEach((rect, index) => {
      const frame = indexed[index];
      if (!frame || index === 0) return;
      const ordinals = ordinalsFor(frame);
      rect.dataset.frameKey = frame.frame_key;
      rect.dataset.frameKind = frame.node_kind;
      rect.dataset.stepOrdinals = ordinals.join(" ");
      if (ordinals.length === 1) rect.dataset.stepOrdinal = String(ordinals[0]);
      rect.setAttribute("tabindex", "0");
      rect.setAttribute("role", "button");
      rect.setAttribute("aria-pressed", String(frame.frame_key === selectedFrameKey));
      rect.setAttribute(
        "aria-label",
        `${frame.display_label}; ${frame.calls} call${frame.calls === 1 ? "" : "s"}; ${fmtUsd(frame.micros)} cumulative; select frame`
      );
      const choose = () => chooseFrame(frame);
      rect.addEventListener("click", choose);
      rect.addEventListener("keydown", (event) => {
        if (event.key !== "Enter" && event.key !== " ") return;
        event.preventDefault();
        choose();
      });
      if (normalizedQuery) {
        const match = `${frame.display_label} ${frame.node_kind} ${frame.cache_class ?? ""}`.toLowerCase().includes(normalizedQuery);
        rect.classList.toggle("is-search-match", match);
        rect.classList.toggle("is-search-muted", !match);
      }
    });

    const allRows = profileTableRows(transformed, weight, tableMode, query, order === "sandwich");
    const shownRows = allRows.slice(0, 100);
    const roleHeader = order === "sandwich" ? el("th", { text: "Role" }) : null;
    const bodyRows = shownRows.map((row) => {
      const ordinals = row.references
        .filter((ref) => ref.run_id === flame.run_id)
        .map((ref) => ref.step_ordinal)
        .filter((value, index, all) => all.indexOf(value) === index)
        .sort((a, b) => a - b);
      return el("tr", {
        "data-profile-row": row.id,
        "data-frame-key": row.frame_key,
        "data-step-ordinals": ordinals.join(" "),
      }, [
        roleHeader ? el("td", { class: "sub", text: humanizeKey(row.role ?? "") }) : null,
        el("td", {}, [
          el("button", {
            class: "run-profile-profile-row-select",
            type: "button",
            text: row.label,
            "aria-label": `Select ${row.label}`,
            onClick: () => chooseFrame(row.frame),
          }),
          el("span", { class: "caption sub", text: humanizeKey(row.node_kind) }),
        ]),
        el("td", { class: "num", text: fmtUsd(row.self_micros) }),
        el("td", { class: "num", text: fmtUsd(row.cum_micros) }),
        el("td", { class: "num", text: fmtTokens(row.calls) }),
        el("td", { class: "num", text: fmtUsd(row.cost_per_call_micros) }),
        el("td", { class: "num", text: fmtTokens(row.self_tokens) }),
        el("td", { class: "num", text: fmtTokens(row.cum_tokens) }),
      ]);
    });
    tableHost.replaceChildren(
      el("div", { class: "run-profile-profile-table-head" }, [
        el("h3", { text: tableMode === "flat" ? "Frames ranked by self" : "Frames ranked by cumulative" }),
        el("span", {
          class: "caption sub",
          text: allRows.length > shownRows.length ? `Showing 100 of ${allRows.length}` : `${allRows.length} frame${allRows.length === 1 ? "" : "s"}`,
        }),
      ]),
      el("div", { class: "run-profile-profile-table-scroll" }, [
        el("table", { class: "data run-profile-cost-table" }, [
          el("thead", {}, [el("tr", {}, [
            roleHeader,
            el("th", { text: "Frame" }),
            el("th", { class: "num", text: "Self cost" }),
            el("th", { class: "num", text: "Cumulative cost" }),
            el("th", { class: "num", text: "Calls" }),
            el("th", { class: "num", text: "Cost / call" }),
            el("th", { class: "num", text: "Self tokens" }),
            el("th", { class: "num", text: "Cumulative tokens" }),
          ])]),
          el("tbody", {}, bodyRows),
        ]),
      ])
    );
    updateCrossHighlights();
  };

  weightButtons.forEach((button) => button.addEventListener("click", () => {
    weight = button.dataset.profileWeight as FlameWeight;
    paint();
  }));
  orderButtons.forEach((button) => button.addEventListener("click", () => {
    order = button.dataset.profileOrder as ProfileOrder;
    frameAction = null;
    paint();
  }));
  tableButtons.forEach((button) => button.addEventListener("click", () => {
    tableMode = button.dataset.profileTable as ProfileTableMode;
    paint();
  }));
  componentSelect.addEventListener("change", () => {
    componentKey = componentSelect.value;
    frameAction = null;
    paint();
  });
  search.addEventListener("input", () => {
    query = search.value;
    paint();
  });
  frameSelect.addEventListener("change", () => {
    frameTargetKey = frameSelect.value;
    selectedFrameKey = frameTargetKey || null;
    frameAction = null;
    paint();
  });
  actionButtons.forEach((button) => button.addEventListener("click", () => {
    if (!frameTargetKey) return;
    frameAction = {
      action: button.dataset.profileAction as ProfileFrameAction,
      frameKey: frameTargetKey,
    };
    status.textContent = `${humanizeKey(frameAction.action)} applied to the selected frame.`;
    paint();
  }));
  clearAction.addEventListener("click", () => {
    frameAction = null;
    status.textContent = "Profiler action cleared.";
    paint();
  });
  selection.subscribe((ordinal) => {
    selectedOrdinal = ordinal;
    updateCrossHighlights();
  });
  paint();
  return panel;
}

function transcriptBody(label: string, text: string): HTMLElement {
  const pre = el("pre", { class: "run-profile-transcript-body" });
  // Captured text remains inert even when it contains HTML/script-looking bytes.
  pre.appendChild(document.createTextNode(text));
  return el("section", { class: "run-profile-transcript-pane" }, [
    el("h4", { text: label }),
    pre,
  ]);
}

function inspectPanel(client: TareClient, runId: string, steps: RunStep[]): HTMLElement {
  const list = el("div", { class: "run-profile-inspect-list" });
  const purgeStatus = el("p", {
    class: "caption sub run-profile-purge-status",
    role: "status",
    "aria-live": "polite",
  });
  let purgeArmed = false;
  let purgeTimer: ReturnType<typeof setTimeout> | undefined;
  const purge = el("button", {
    class: "btn danger run-profile-purge",
    type: "button",
    text: "Purge all stored bodies",
  }) as HTMLButtonElement;

  for (const step of steps) {
    const result = el("div", {
      class: "run-profile-transcript",
      "data-transcript-result": String(step.ordinal),
      role: "region",
      "aria-live": "polite",
    });
    const load = el("button", {
      class: "btn run-profile-inspect-load",
      type: "button",
      "data-inspect-step": String(step.ordinal),
      text: "Load stored bodies",
    }) as HTMLButtonElement;
    load.addEventListener("click", () => {
      load.disabled = true;
      load.textContent = "Loading…";
      result.replaceChildren(el("p", { class: "caption sub", text: "Loading locally stored bodies…" }));
      void client
        .transcript(runId, step.ordinal)
        .then((transcript) => {
          if (!transcript) {
            load.textContent = "No stored bodies";
            result.replaceChildren(
              emptyState(
                "No bodies captured for this step",
                "The run used Max inspect, but this capture path or step did not persist a request/response body."
              )
            );
            return;
          }
          load.textContent = "Bodies loaded";
          result.replaceChildren(
            transcript.truncated === true
              ? el("p", {
                  class: "run-profile-transcript-truncated",
                  role: "note",
                  text: "Body was truncated at the capture cap; the text below is incomplete.",
                })
              : transcript.truncated === false
                ? el("p", {
                  class: "caption sub",
                  text: "Capture reported both stored bodies complete within the cap.",
                })
                : el("p", {
                    class: "run-profile-transcript-truncated",
                    role: "note",
                    text: "Legacy capture did not preserve truncation evidence; the text below may be incomplete.",
                  }),
            transcriptBody("Redacted request", transcript.req),
            transcriptBody("Redacted response", transcript.resp)
          );
        })
        .catch((error) => {
          load.disabled = false;
          load.textContent = "Retry loading bodies";
          result.replaceChildren(errorNode("Couldn't load stored bodies", error));
        });
    });
    list.appendChild(
      el("section", { class: "run-profile-inspect-step" }, [
        el("div", { class: "run-profile-inspect-step-head" }, [
          el("h3", { text: `Step ${step.ordinal}` }),
          load,
        ]),
        result,
      ])
    );
  }

  purge.addEventListener("click", () => {
    if (!purgeArmed) {
      purgeArmed = true;
      purge.textContent = "Confirm purge all bodies";
      purgeStatus.textContent = "Click again to permanently purge every locally stored request/response body.";
      purgeTimer = setTimeout(() => {
        purgeArmed = false;
        purge.textContent = "Purge all stored bodies";
        purgeStatus.textContent = "Purge confirmation expired.";
      }, 5_000);
      return;
    }
    purgeArmed = false;
    if (purgeTimer) clearTimeout(purgeTimer);
    purge.disabled = true;
    purge.textContent = "Purging…";
    purgeStatus.textContent = "Securely purging the separate local transcript store…";
    void client
      .purgeTranscripts()
      .then(() => {
        purge.textContent = "Bodies purged";
        purgeStatus.textContent = "All locally stored bodies were purged. Counts and hashes remain.";
        for (const result of Array.from(list.querySelectorAll<HTMLElement>("[data-transcript-result]"))) {
          result.replaceChildren(el("p", { class: "caption sub", text: "Purged from the local transcript store." }));
        }
        for (const button of Array.from(list.querySelectorAll<HTMLButtonElement>("[data-inspect-step]"))) {
          button.disabled = true;
          button.textContent = "Purged";
        }
      })
      .catch((error) => {
        purge.disabled = false;
        purge.textContent = "Purge all stored bodies";
        purgeStatus.textContent = `Purge failed; stored bodies may remain: ${String(error)}`;
      });
  });

  return el("section", { class: "run-profile-inspect", "aria-labelledby": "run-profile-inspect-title" }, [
    el("div", { class: "run-profile-inspect-heading" }, [
      el("h2", { id: "run-profile-inspect-title", text: "Inspect stored bodies" }),
      el("span", { class: "chip", text: "Max inspect" }),
    ]),
    el("div", { class: "run-profile-privacy-warning", role: "note" }, [
      el("strong", { text: "Potentially sensitive local data. " }),
      "These bodies are locally stored, best-effort redacted, capped, and potentially sensitive. Review before copying or sharing.",
    ]),
    el("p", {
      class: "caption",
      text: "Nothing below is fetched until you explicitly load one step. Purge removes all bodies from the separate transcript store, not the counts ledger.",
    }),
    list,
    el("div", { class: "run-profile-purge-actions" }, [purge, purgeStatus]),
  ]);
}

function shapePanel(
  client: TareClient,
  runId: string,
  steps: RunStep[],
  meta: RunMeta | null
): HTMLElement {
  const panel = el("section", { class: "run-profile-tab-content" }, [
    el("h2", { text: "Prompt shape" }),
    el("p", {
      class: "caption",
      text: "Stored counts, component labels, cache/config facts, and opaque hashes only. Prompt and response text are not read to build this view.",
    }),
    el("p", {
      class: "run-profile-shape-assumption",
      role: "note",
      text: "Assumptions: byte weights are structural attribution inputs, not token counts; hashes are local fingerprints, not content; a cache-controlled component records request intent, not a confirmed cache hit.",
    }),
  ]);
  const anatomy = steps.filter((step) => step.anatomy);
  if (anatomy.length === 0) {
    panel.appendChild(emptyState("Shape metadata unavailable", "This capture profile did not record prompt anatomy."));
  }
  for (const step of anatomy) {
    const a = step.anatomy!;
    const configRows: Array<[string, string | Node, string]> = [
      ["Provider", step.provider || "Not recorded", "Stored step label"],
      ["Model", step.model || "Not recorded", "Stored request metadata"],
      ["Request mode", a.stream == null ? "Not recorded" : a.stream ? "Streaming" : "Non-streaming", "Stored request shape"],
      ["Cache control", a.cache_control == null ? "Not recorded" : a.cache_control ? "Declared" : "Not declared", "Stored request intent"],
      ["Cache TTL hint", a.ttl === "1h" ? "1 hour" : a.ttl === "5m" ? "5 minutes" : "Not recorded", "Request hint; provider usage split is authoritative"],
      ["Reasoning effort", a.effort || "Not recorded", "Allow-listed request config"],
      ["System hash", a.system_hash ? el("code", { text: a.system_hash }) : "Withheld or not recorded", "Short stable local fingerprint"],
      ["Request hash", a.request_hash ? el("code", { text: a.request_hash }) : "Withheld or not recorded", "Short stable local fingerprint"],
    ];
    panel.appendChild(
      el("section", { class: "run-profile-shape-step" }, [
        el("h3", { text: `Step ${step.ordinal}` }),
        el("p", { class: "caption", text: `${fmtTokens(a.total_bytes)} captured structural bytes` }),
        el("dl", { class: "run-profile-shape-facts" }, configRows.flatMap(([term, value, source]) => [
          el("dt", { text: term }),
          el("dd", {}, [value, el("span", { class: "caption sub", text: source })]),
        ])),
        el("dl", { class: "run-profile-cache-facts", "aria-label": `Step ${step.ordinal} captured usage counters` }, [
          el("div", {}, [el("dt", { text: "Fresh input" }), el("dd", { class: "num", text: fmtTokens(step.fresh_input) })]),
          el("div", {}, [el("dt", { text: "Cache read" }), el("dd", { class: "num", text: fmtTokens(step.cache_read) })]),
          el("div", {}, [el("dt", { text: "Cache write" }), el("dd", { class: "num", text: fmtTokens(step.cache_write) })]),
          el("div", {}, [el("dt", { text: "Output" }), el("dd", { class: "num", text: fmtTokens(step.output) })]),
        ]),
        el(
          "ul",
          { class: "run-profile-shape-list" },
          a.components.map((component) =>
            el("li", {}, [
              el("span", { text: component.label || humanizeKey(component.component) }),
              el("span", {
                class: "num sub",
                text: `${fmtTokens(component.bytes)} bytes${component.cached ? " · cache-controlled component" : ""}`,
              }),
            ])
          )
        ),
      ])
    );
  }
  // Fail closed: only the profile persisted on this run can expose the Inspect controls. Current
  // app config is not enough because changing it later cannot retroactively change what this run
  // captured. Counts-only/unknown profiles receive no empty or actionable body surface at all.
  if (meta?.profile === "max_inspect") panel.appendChild(inspectPanel(client, runId, steps));
  return panel;
}

function provenancePanel(meta: RunMeta | null): HTMLElement {
  const panel = el("section", { class: "run-profile-tab-content" }, [
    el("h2", { text: "Provenance" }),
    el("p", {
      class: "caption",
      text: "These fields describe captured rows and pricing inputs. They do not claim complete provider capture.",
    }),
  ]);
  if (!meta) {
    panel.appendChild(emptyState("Provenance unavailable", "No run metadata was returned."));
    return panel;
  }
  const rows: Array<[string, string, string, string]> = [
    ["Captured date", meta.created_date, "Stored run row", "Capture-date bucket; legacy rows may not contain an instant that can be re-bucketed."],
    ["Capture sources", meta.sources.join(", ") || "Not recorded", "Distinct stored step labels", "Completeness unknown; recorded sources do not prove all provider traffic was captured."],
    ["Providers", meta.providers.join(", ") || "Not recorded", "Distinct stored step labels", "A proxy path may classify provider from the request route."],
    ["Models", meta.models.join(", ") || "Not recorded", "Distinct stored request metadata", "Opaque model IDs are shown as captured; aliases are not normalized here."],
    ["Privacy profile", meta.profile || "Not recorded", "Persisted effective capture profile", "Controls what this run could retain; current Settings do not rewrite historical capture."],
    ["Privacy policy", meta.privacy_policy_id || "Not recorded", "Persisted policy fingerprint", "Identifies capture policy, not proof that capture coverage was complete."],
    ["Pricing edition", `${meta.pricing_version} · effective ${meta.effective_date}`, "Read-time local pricing table", "Estimated from captured usage; not a provider invoice or billed total."],
    ["Stop reasons", meta.stop_reasons.join(", ") || "Not recorded", "Distinct captured provider values", "Missing values are omitted and never inferred."],
    ["Coverage", "Unknown", "No provider-total denominator in run metadata", "Do not interpret captured rows as 100% of external usage."],
  ];
  panel.appendChild(
    el("div", {
      class: "run-profile-provenance-wrap",
      tabindex: "0",
      role: "region",
      "aria-label": "Run provenance table; scroll horizontally for all columns",
    }, [
      el("table", { class: "data run-profile-provenance" }, [
        el("thead", {}, [el("tr", {}, [
          el("th", { text: "Field" }),
          el("th", { text: "Value" }),
          el("th", { text: "Origin" }),
          el("th", { text: "Limit / assumption" }),
        ])]),
        el("tbody", {}, rows.map(([field, value, origin, assumption]) => el("tr", {}, [
          el("th", { scope: "row", text: field }),
          el("td", { text: value }),
          el("td", { text: origin }),
          el("td", { class: "sub", text: assumption }),
        ]))),
      ]),
    ])
  );
  return panel;
}

function tabPanel(
  tab: RunProfileTab,
  client: TareClient,
  runId: string,
  steps: RunStep[],
  timeline: RunTimelineModel,
  flame: FlamegraphModel | null,
  profile: ProfileTable | null,
  meta: RunMeta | null,
  selection: StepSelection
): HTMLElement {
  const panel = el("div", {
    id: "run-profile-panel",
    class: "run-profile-panel",
    role: "tabpanel",
    tabindex: "0",
    "aria-labelledby": `run-profile-tab-${tab}`,
  });
  const content =
    tab === "timeline"
      ? timelinePanel(timeline, selection)
      : tab === "profile"
        ? profilePanel(flame, profile, selection)
        : tab === "shape"
          ? shapePanel(client, runId, steps, meta)
          : provenancePanel(meta);
  panel.appendChild(content);
  return panel;
}

function downloadText(filename: string, content: string): void {
  if (typeof URL.createObjectURL !== "function") return;
  const url = URL.createObjectURL(new Blob([content], { type: "application/octet-stream" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.hidden = true;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

function noteSection(client: TareClient, runId: string, note: RunNote | null): HTMLElement {
  const textarea = el("textarea", {
    class: "run-profile-note",
    rows: "4",
    placeholder: "Your local note about this run",
  }) as HTMLTextAreaElement;
  textarea.value = note?.note_text ?? "";
  const tags = el("input", {
    class: "run-profile-note-tags",
    type: "text",
    value: note?.tags.join(", ") ?? "",
    placeholder: "tags, comma-separated",
    "aria-label": "Run note tags",
  }) as HTMLInputElement;
  const starred = el("input", { type: "checkbox" }) as HTMLInputElement;
  starred.checked = note?.starred ?? false;
  const status = el("span", { class: "caption sub", role: "status", "aria-live": "polite" });
  const save = el("button", {
    class: "btn run-profile-note-save",
    type: "button",
    text: "Save note",
    onClick: () => {
      status.textContent = "Saving…";
      void client
        .saveRunNote({
          run_id: runId,
          note_text: textarea.value,
          tags: tags.value
            .split(",")
            .map((tag) => tag.trim().toLowerCase())
            .filter(Boolean),
          starred: starred.checked,
        })
        .then(() => {
          status.textContent = "Saved locally.";
        })
        .catch((error) => {
          status.textContent = `Couldn't save note: ${String(error)}`;
        });
    },
  });
  return el("section", { class: "run-profile-context-section" }, [
    el("h3", { text: "Notes" }),
    textarea,
    tags,
    el("label", { class: "caption" }, [starred, " Star this run"]),
    el("div", { class: "run-profile-context-actions" }, [save, status]),
  ]);
}

function exportSection(client: TareClient, runId: string): HTMLElement {
  const status = el("p", { class: "caption sub", role: "status", "aria-live": "polite" });
  const formats: Array<[string, string, string]> = [
    ["speedscope", "speedscope.json", "Speedscope"],
    ["otel", "otlp.json", "OTLP trace"],
    ["receipt", "receipt.json", "Receipt"],
  ];
  return el("section", { class: "run-profile-context-section" }, [
    el("h3", { text: "Export" }),
    el(
      "div",
      { class: "run-profile-context-actions" },
      formats.map(([format, extension, label]) =>
        el("button", {
          class: "btn",
          type: "button",
          "data-export": format,
          text: label,
          onClick: () => {
            status.textContent = `Preparing ${label}…`;
            void client
              .exportRun(runId, format)
              .then((content) => {
                downloadText(`${runId}.${extension}`, content);
                status.textContent = `${label} export ready.`;
              })
              .catch((error) => {
                status.textContent = `Export failed: ${String(error)}`;
              });
          },
        })
      )
    ),
    status,
  ]);
}

function attestationSection(client: TareClient, runId: string): HTMLElement {
  const output = el("div", { class: "run-profile-attestation-output", "aria-live": "polite" });
  const maxPrivate = el("input", { type: "checkbox" }) as HTMLInputElement;
  return el("section", { class: "run-profile-context-section" }, [
    el("h3", { text: "Attestation" }),
    el("p", {
      class: "caption",
      text: "Recompute captured tokens × effective pricing offline. This is an estimate receipt, not a billing or cryptographic seal.",
    }),
    el("label", { class: "caption" }, [maxPrivate, " Max-private receipt"]),
    el("button", {
      class: "btn run-profile-attest",
      type: "button",
      text: "Attest + verify",
      onClick: () => {
        output.replaceChildren(el("p", { class: "skeleton", text: "Verifying…" }));
        void client
          .receipt(runId, maxPrivate.checked)
          .then((result) => output.replaceChildren(receiptStatement(result.verify)))
          .catch((error) => output.replaceChildren(errorNode("Attestation failed", error)));
      },
    }),
    output,
  ]);
}

function selectedStepSection(
  steps: RunStep[],
  timeline: RunTimelineModel,
  selection: StepSelection
): HTMLElement {
  const section = el("section", {
    class: "run-profile-context-section run-profile-selected-step",
    "aria-labelledby": "run-profile-selected-step-title",
  });
  selection.subscribe((ordinal) => {
    const step = steps.find((candidate) => candidate.ordinal === ordinal);
    if (!step) {
      section.replaceChildren(
        el("h3", { id: "run-profile-selected-step-title", text: "Selected step" }),
        el("p", { class: "caption sub", text: "Select a timeline row, Beam segment, profile frame, or unique table row to inspect one step." })
      );
      return;
    }
    const timed = timeline.timedByOrdinal.get(step.ordinal);
    const overlaps = relatedSteps(timeline, step.ordinal, "overlap");
    const nested = relatedSteps(timeline, step.ordinal, "nested");
    const concurrent = relatedSteps(timeline, step.ordinal, "concurrent");
    const stepList = (ordinals: number[]): string => {
      const shown = ordinals.slice(0, 5).map((value) => `Step ${value}`).join(", ");
      return ordinals.length > 5 ? `${shown} +${ordinals.length - 5} more` : shown;
    };
    let relationship = "Concurrency not claimed";
    if (concurrent.length > 0) relationship = `Concurrent sibling span${concurrent.length === 1 ? "" : "s"}: ${stepList(concurrent)}`;
    else if (nested.length > 0) relationship = `Nested span overlap: ${stepList(nested)}`;
    else if (overlaps.length > 0) relationship = `Timestamp overlap with ${stepList(overlaps)} · no sibling-span relationship`;
    else if (timed) relationship = "No timestamp overlap observed";
    const rows: Array<[string, string | Node]> = [
      ["Model", step.model || "Not recorded"],
      ["Provider", step.provider || "Not recorded"],
      ["Est. priced spend", step.micros === 0 && step.tokens > 0 ? "Unpriced" : fmtUsd(step.micros)],
      ["Tokens", fmtTokens(step.tokens)],
      ["Local latency", step.duration_ms && step.duration_ms > 0 ? fmtDuration(step.duration_ms) : "Not captured"],
      ["Timeline start", timed ? `+${formatTimelineNanos(timed.offset)}` : "Not captured · order only"],
      ["Timed span", timed ? formatTimelineNanos(timed.duration) : "Not captured"],
      ["Overlap evidence", relationship],
    ];
    if (step.trace_id) rows.push(["Trace ID", el("code", { text: step.trace_id })]);
    if (step.span_id) rows.push(["Span ID", el("code", { text: step.span_id })]);
    if (step.parent_span_id) rows.push(["Parent span", el("code", { text: step.parent_span_id })]);
    section.replaceChildren(
      el("h3", { id: "run-profile-selected-step-title", text: `Selected Step ${step.ordinal}` }),
      el("dl", { class: "run-profile-selected-step-facts", "aria-live": "polite" },
        rows.flatMap(([term, value]) => [el("dt", { text: term }), el("dd", {}, [value])])
      )
    );
  });
  return section;
}

function contextualInspector(
  client: TareClient,
  runId: string,
  note: RunNote | null,
  meta: RunMeta | null,
  status: RunStatus,
  steps: RunStep[],
  timeline: RunTimelineModel,
  selection: StepSelection
): HTMLElement {
  return el("aside", {
    id: "run-profile-inspector",
    class: "run-profile-inspector",
    "data-pane": "inspector",
    "aria-label": "Persistent run context",
  }, [
    el("div", { class: "run-profile-inspector-head" }, [
      el("h2", { text: "Run context" }),
      el("code", { text: runId }),
      el("p", {
        class: "caption sub",
        text: `${meta?.created_date ?? "Date not recorded"} · ${status.steps} step${status.steps === 1 ? "" : "s"}`,
      }),
    ]),
    selectedStepSection(steps, timeline, selection),
    noteSection(client, runId, note),
    exportSection(client, runId),
    attestationSection(client, runId),
  ]);
}

/// Render the canonical Run Profile route. Critical run status is all-or-nothing: if it fails, show
/// an explicit error instead of composing $0/default metadata that could mislead. Supplementary
/// contracts degrade locally inside their relevant tab/summary.
export async function renderRunProfile(
  root: HTMLElement,
  client: TareClient,
  route: Route,
  context?: WorkspaceContext
): Promise<void> {
  root.querySelector<HTMLElement>("[data-adaptive-panes]")?.dispatchEvent(new Event("tare:dispose"));
  const runId = route.segments[2];
  if (!runId) {
    root.replaceChildren(
      errorNode("This Run Profile link is missing a run id.", "Missing run id", {
        actions: [{ label: "Back to runs", href: "#/investigate?entity=runs", primary: true }],
      })
    );
    return;
  }
  root.replaceChildren(el("div", { class: "run-profile-loading skeleton", text: "Loading Run Profile…" }));

  let status: RunStatus;
  try {
    status = await client.runStatus(runId);
  } catch (error) {
    // A failed status read can mean either an unavailable service or an obsolete deep link. Ask the
    // authoritative id list before choosing the recovery copy; never call a missing record an outage.
    const availableRunIds = await client.listRuns().catch(() => null);
    const missing = availableRunIds !== null && !availableRunIds.includes(runId);
    if (missing) {
      forgetRunReferences(runId);
      if (context?.analysis.get().pinned?.kind === "run" && context.analysis.get().pinned?.id === runId) {
        context.analysis.setPinned(null);
      }
    }
    root.replaceChildren(
      errorNode(missing
        ? `Run ${runId} is no longer available in the active capture store.`
        : `Couldn't load Run Profile for ${runId}. Check that capture is running.`, error, {
        actions: [
          { label: "Retry", primary: true, run: () => renderRunProfile(root, client, route, context) },
          { label: "Back to runs", href: "#/investigate?entity=runs" },
        ],
      })
    );
    return;
  }

  const state = context?.analysis.get();
  const baselinePromise = state?.baseline
    ? client.resolveCohort(state.baseline.cohort).catch(() => null)
    : Promise.resolve(null);
  const [meta, steps, autopsy, profile, flame, note, frontier, baselineResult, runIds] = await Promise.all([
    client.runMeta(runId).catch(() => null),
    client.runSteps(runId).catch(() => [] as RunStep[]),
    client.sessionAutopsy(runId).catch(() => null),
    client.profile(runId, "cum").catch(() => null),
    client.flamegraph(runId).catch(() => null),
    client.getRunNote(runId).catch(() => null),
    client.frontier().catch(() => null),
    baselinePromise,
    client.listRuns().catch(() => null),
  ]);

  // Opening the canonical route pins the entity in the shared interaction spine without mutating
  // scope, Selection A, or Baseline B. A selected step survives routed tabs for this same run only;
  // opening another run resets it rather than accidentally highlighting the same ordinal there.
  const retainedStep = focusStepForRun(context, runId, steps);
  context?.analysis.setPinned({ kind: "run", id: runId, label: runId });
  if (retainedStep == null) {
    context?.analysis.setFocus({
      pane: "canvas",
      highlighted: { kind: "run", id: runId, label: runId },
      stepOrdinal: undefined,
      framePath: undefined,
    });
  }
  try {
    recordOpenedRun(runId); // keep the compatibility sidebar recents useful during the cutover.
  } catch {
    // Storage failure cannot make locally captured evidence unavailable.
  }

  const quality = frontier?.points.find((point) => point.run_id === runId)?.quality;
  const baseline = baselineSummary(state, status, baselineResult);
  const tab = activeTab(route);
  const query = route.query ?? {};
  const timeline = buildRunTimeline(steps);
  const selection = createStepSelection(context, runId, steps);

  const header = el("header", { class: "run-profile-header" }, [
    el("div", { class: "run-profile-nav" }, [
      el("a", {
        class: "run-profile-back",
        href: routePath(["investigate"], withoutView(query)),
        text: "← Investigation",
      }),
      el("div", {}, [
        el("p", { class: "eyebrow", text: "Run Profile" }),
        el("h2", { class: "run-profile-title", text: runId }),
      ]),
      el("button", {
        class: "btn pane-inspector-trigger",
        type: "button",
        "data-pane-inspector": "",
        "aria-controls": "run-profile-inspector",
        "aria-expanded": "false",
        text: "Explain selection",
      }),
    ]),
    stickySummary(status, meta, steps, autopsy, quality, baseline),
  ]);
  const canvas = el("section", {
    id: "run-profile-canvas",
    class: "run-profile-canvas",
    "data-pane": "canvas",
    "aria-label": `Analysis for run ${runId}`,
  }, [
    whySection(status, steps, autopsy, selection),
    tabBar(route, tab),
    tabPanel(tab, client, runId, steps, timeline, flame, profile, meta, selection),
  ]);
  mountRunWorkbench(
    root,
    header,
    runEntityPane(runIds, runId, route),
    canvas,
    contextualInspector(client, runId, note, meta, status, steps, timeline, selection),
    context,
    "canvas",
    selection
  );
}
