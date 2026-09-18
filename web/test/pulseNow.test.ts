// Pulse "Now" feed. Verifies the acceptance: bounded + cancellable polling, selection
// survives updates, honest liveness caveats, and one feed surface (no configurable card registry).

import { describe, it, expect, vi, afterEach } from "vitest";
import {
  buildNowSection,
  reconcileFeed,
  pollNowFeed,
  renderNowFeed,
  type NowState,
} from "../src/workspaces/pulseNow.js";
import { fakeClient } from "./fakeClient.js";
import type { SessionLive, RecentStep } from "../src/client.js";

afterEach(() => vi.useRealTimers());

function sessions(n: number): SessionLive[] {
  return Array.from({ length: n }, (_, i) => ({
    session: `s${i}`,
    source: "claude",
    state: i % 2 ? "idle" : "working",
    last_seen_age_s: i * 3,
    events: 5,
    last_model: "sonnet",
    micros: (n - i) * 1_000_000, // descending so ordering is observable
  }));
}
function steps(n: number): RecentStep[] {
  return Array.from({ length: n }, (_, i) => ({
    run_id: `r${i}`,
    ordinal: 0,
    provider: "anthropic",
    model: "sonnet",
    fresh_input: 0,
    cache_read: 0,
    cache_write: 0,
    output: 0,
    reasoning: 0,
    tokens: 1000,
    micros: (n - i) * 500_000,
    stop_reason: null,
  })) as RecentStep[];
}

const st = (): NowState => ({ selected: null });

describe("Now feed — reconcile (pure)", () => {
  it("is bounded: caps sessions + steps to the feed limit regardless of input size", () => {
    const sec = buildNowSection();
    reconcileFeed(sec, sessions(50), steps(50), st());
    expect(sec.querySelectorAll(".pulse-now-sessions li[data-key]").length).toBe(8);
    expect(sec.querySelectorAll(".pulse-now-steps li[data-key]").length).toBe(8);
  });

  it("renders sentence rows as links (one feed surface), never configurable cards", () => {
    const sec = buildNowSection();
    reconcileFeed(sec, sessions(2), steps(2), st());
    const rows = sec.querySelectorAll(".pulse-now-row");
    expect(rows.length).toBe(4);
    for (const r of Array.from(rows)) {
      expect(r.tagName).toBe("A");
      expect(r.getAttribute("href")).toMatch(/^#\//);
      expect(r.classList.contains("card")).toBe(false);
    }
    expect(sec.querySelector(".card-registry, .card-menu")).toBeNull();
  });

  it("states honest liveness caveats (recency-derived), not a real-time guarantee", () => {
    const live = buildNowSection().querySelector(".pulse-now-live")!;
    expect(live.textContent).toMatch(/recency-derived/);
    expect(live.textContent).toMatch(/idle-but-open session may read as ended/);
  });

  it("honest empty states when nothing is live", () => {
    const sec = buildNowSection();
    reconcileFeed(sec, [], [], st());
    expect(sec.querySelector(".pulse-now-sessions")?.textContent).toMatch(/No active sessions/);
    expect(sec.querySelector(".pulse-now-steps")?.textContent).toMatch(/No recent steps/);
  });

  it("selection SURVIVES a poll update (keyed reconcile re-applies the selected row)", () => {
    const sec = buildNowSection();
    const state = st();
    reconcileFeed(sec, sessions(3), steps(0), state);
    const row = sec.querySelector<HTMLElement>(".pulse-now-sessions .pulse-now-row")!;
    row.click(); // select it
    expect(row.classList.contains("selected")).toBe(true);
    const key = row.dataset.key;
    // A fresh poll with new data (spend changed) must keep the same row selected + its node identity.
    reconcileFeed(sec, sessions(3), steps(0), state);
    const after = sec.querySelector<HTMLElement>(`.pulse-now-sessions li[data-key="${key}"] .pulse-now-row`)!;
    expect(after.classList.contains("selected")).toBe(true);
    expect(after.getAttribute("aria-current")).toBe("true");
  });
});

describe("Now feed — poll (async + bounded/cancellable wiring)", () => {
  it("pollNowFeed populates from the live client surfaces", async () => {
    const sec = buildNowSection();
    await pollNowFeed(sec, fakeClient({ sessionsLive: async () => sessions(2), recentSteps: async () => steps(2) }), st());
    expect(sec.querySelectorAll(".pulse-now-row").length).toBe(4);
  });

  it("a transient poll failure keeps the last good feed (never blanks Pulse)", async () => {
    const sec = buildNowSection();
    await pollNowFeed(sec, fakeClient({ sessionsLive: async () => sessions(1), recentSteps: async () => steps(0) }), st());
    expect(sec.querySelectorAll(".pulse-now-sessions .pulse-now-row").length).toBe(1);
    await pollNowFeed(sec, fakeClient({ sessionsLive: async () => { throw new Error("blip"); } }), st());
    expect(sec.querySelectorAll(".pulse-now-sessions .pulse-now-row").length).toBe(1); // unchanged
  });

  it("the poll is bounded (one interval) and self-cancels when the section leaves the DOM", async () => {
    vi.useFakeTimers();
    let calls = 0;
    const client = fakeClient({
      sessionsLive: async () => {
        calls++;
        return sessions(1);
      },
      recentSteps: async () => steps(0),
    });
    const sec = renderNowFeed(client, { intervalMs: 1000 });
    document.body.appendChild(sec); // connected → interval keeps polling
    await vi.advanceTimersByTimeAsync(1000);
    await vi.advanceTimersByTimeAsync(1000);
    const whileConnected = calls;
    expect(whileConnected).toBeGreaterThanOrEqual(2); // initial + ticks
    sec.remove(); // detach → next fire must self-cancel
    await vi.advanceTimersByTimeAsync(5000);
    expect(calls).toBe(whileConnected); // no further polls after detach
  });
});
