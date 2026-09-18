// Pulse "Now" feed. ONE polling surface — active agent sessions +
// recent expensive steps — replacing the old Live card wall (no configurable card registry). The poll
// is BOUNDED (capped rows, single interval) and CANCELLABLE (self-cancels the moment its section
// leaves the DOM on navigation), selection SURVIVES updates (keyed in-place reconcile re-applies the
// selected row), and liveness is stated HONESTLY (recency-derived; an idle-but-open session can read
// as ended because no CLI emits a session-end event). Framework-free + jsdom-testable: the pure
// reconcile + the async poll are exported separately from the timer wiring.

import { el } from "../ui/el.js";
import { fmtUsd, fmtTokens } from "../ui/format.js";
import { routePath } from "../ui/store.js";
import { refreshMs } from "../ui/prefs.js";
import type { TareClient, SessionLive, RecentStep } from "../client.js";

const MAX_SESSIONS = 8;
const MAX_STEPS = 8;

/// Mutable feed state that must survive a poll update: the selected row key. Kept out of the DOM so
/// a re-render re-applies it.
export interface NowState {
  selected: string | null;
}

const sessionKey = (s: SessionLive): string => `session:${s.source}/${s.session}`;
const stepKey = (s: RecentStep): string => `step:${s.run_id}#${s.ordinal}`;

/// Build the stable Now-feed skeleton once; the poll updates the two lists + the liveness caption in
/// place (never rebuilding the section, so focus/selection/scroll survive).
export function buildNowSection(): HTMLElement {
  return el("section", { class: "pulse-now", "aria-labelledby": "pulse-now-h" }, [
    el("div", { class: "pulse-now-head" }, [
      el("h2", { id: "pulse-now-h", class: "subhead", text: "Now" }),
      // Honest liveness caveat — recency-derived, not a real-time guarantee.
      el("p", {
        class: "pulse-now-live caption sub",
        "aria-live": "polite",
        text: `Live · auto-refresh ${Math.round(refreshMs() / 1000)}s · liveness is recency-derived (an idle-but-open session may read as ended).`,
      }),
    ]),
    el("h3", { class: "pulse-now-subhead sub", text: "Active sessions" }),
    el("ol", { class: "pulse-now-sessions pulse-now-list" }),
    el("h3", { class: "pulse-now-subhead sub", text: "Recent expensive steps" }),
    el("ol", { class: "pulse-now-steps pulse-now-list" }),
  ]);
}

/// Apply a click-to-select to a feed row: toggles the shared selection so exactly one row is selected,
/// and persists it in `state` so the next reconcile re-applies it.
function wireSelect(row: HTMLElement, key: string, section: HTMLElement, state: NowState): void {
  row.addEventListener("click", () => {
    state.selected = state.selected === key ? null : key;
    for (const r of Array.from(section.querySelectorAll<HTMLElement>(".pulse-now-row"))) {
      r.classList.toggle("selected", r.dataset.key === state.selected);
      r.setAttribute("aria-current", r.dataset.key === state.selected ? "true" : "false");
    }
  });
}

/// Reconcile the two feed lists IN PLACE from fresh data (pure DOM; deterministic — no clock/random).
/// Bounded to the cap, keyed so the selected row survives, honest empty states. Exported for tests.
export function reconcileFeed(
  section: HTMLElement,
  sessions: SessionLive[],
  steps: RecentStep[],
  state: NowState
): void {
  const sessList = section.querySelector<HTMLElement>(".pulse-now-sessions")!;
  const stepList = section.querySelector<HTMLElement>(".pulse-now-steps")!;

  const reconcile = (
    list: HTMLElement,
    rows: Array<{ key: string; href: string; text: string; dollars: number }>,
    emptyText: string
  ): void => {
    if (rows.length === 0) {
      list.replaceChildren(el("li", { class: "pulse-now-empty caption sub", text: emptyText }));
      return;
    }
    const existing = new Map<string, HTMLElement>();
    for (const li of Array.from(list.querySelectorAll<HTMLElement>("li[data-key]"))) {
      existing.set(li.dataset.key!, li);
    }
    const next: HTMLElement[] = [];
    for (const r of rows) {
      let li = existing.get(r.key);
      if (!li) {
        const a = el("a", { class: "pulse-now-row", href: r.href }, [
          el("span", { class: "pulse-now-text" }),
          el("span", { class: "pulse-now-dollars num" }),
        ]);
        a.dataset.key = r.key;
        wireSelect(a, r.key, section, state);
        li = el("li", {}, [a]);
        li.dataset.key = r.key;
      }
      // Update fields in place (keyed) so selection + node identity survive the poll.
      const a = li.querySelector<HTMLElement>(".pulse-now-row")!;
      a.setAttribute("href", r.href);
      a.querySelector(".pulse-now-text")!.textContent = r.text;
      a.querySelector(".pulse-now-dollars")!.textContent = fmtUsd(r.dollars);
      a.classList.toggle("selected", state.selected === r.key);
      a.setAttribute("aria-current", state.selected === r.key ? "true" : "false");
      next.push(li);
    }
    list.replaceChildren(...next);
  };

  reconcile(
    sessList,
    sessions.slice(0, MAX_SESSIONS).map((s) => ({
      key: sessionKey(s),
      href: routePath(["investigate"], { entity: "sessions" }),
      text: `${s.session} · ${s.state} · ${s.last_model || "—"} · last seen ${Math.round(s.last_seen_age_s)}s ago`,
      dollars: s.micros,
    })),
    "No active sessions right now."
  );
  reconcile(
    stepList,
    steps.slice(0, MAX_STEPS).map((s) => ({
      key: stepKey(s),
      href: routePath(["investigate", "run", s.run_id]),
      text: `${s.model} · ${fmtTokens(s.tokens)} tokens · run ${s.run_id}`,
      dollars: s.micros,
    })),
    "No recent steps captured yet."
  );
}

/// One poll: fetch the live surfaces + reconcile. Async + awaitable so tests can drive a tick without
/// timers. Errors are swallowed at the feed boundary (a transient poll failure must not blank Pulse).
export async function pollNowFeed(section: HTMLElement, client: TareClient, state: NowState): Promise<void> {
  try {
    const [sessions, steps] = await Promise.all([client.sessionsLive(), client.recentSteps(MAX_STEPS)]);
    reconcileFeed(section, sessions, steps, state);
  } catch {
    /* keep the last good feed; the caption already states liveness is best-effort */
  }
}

/// Build the Now feed and start its bounded, self-cancelling poll. Returns the section (appended into
/// Pulse). The interval stops the moment the section detaches (navigation) — nothing leaks. `opts`
/// lets tests inject the interval; production uses the user's refresh cadence.
export function renderNowFeed(client: TareClient, opts: { intervalMs?: number } = {}): HTMLElement {
  const section = buildNowSection();
  const state: NowState = { selected: null };
  void pollNowFeed(section, client, state); // initial paint
  const id = setInterval(() => {
    if (!section.isConnected) {
      clearInterval(id); // cancellable: self-cancels when the pane leaves the DOM
      return;
    }
    void pollNowFeed(section, client, state);
  }, opts.intervalMs ?? refreshMs());
  return section;
}
