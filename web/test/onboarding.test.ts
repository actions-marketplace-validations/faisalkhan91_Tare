import { describe, it, expect, vi, afterEach, beforeEach } from "vitest";
import { renderOnboarding } from "../src/screens/onboarding.js";
import { fakeClient } from "./fakeClient.js";

// Onboarding persists its own state; reset only those keys so parallel suites' compatibility
// fixtures are not erased when Node's file-backed localStorage is shared across workers.
beforeEach(() => {
  localStorage.removeItem("tare-onboarded");
  localStorage.removeItem("tare-onboard-step");
  localStorage.removeItem("tare-provider");
});

const flush = (): Promise<void> => new Promise((r) => setTimeout(r, 0));
const btnByText = (root: HTMLElement, text: string): HTMLButtonElement | undefined =>
  Array.from(root.querySelectorAll("button")).find((b) => b.textContent === text) as
    | HTMLButtonElement
    | undefined;

afterEach(() => {
  document.body.replaceChildren();
  vi.useRealTimers();
});

describe("onboarding: start capture service", () => {
  it("shows in-flight then a visible inline error on failure, re-armed for retry", async () => {
    const client = fakeClient({
      canControlProxy: () => true,
      proxyStart: async () => {
        throw new Error("port 8788 busy");
      },
    });
    const root = document.createElement("div");
    document.body.appendChild(root);
    await renderOnboarding(root, client);
    const btn = btnByText(root, "Start capture service")!;
    expect(btn).toBeTruthy();
    btn.click();
    // Synchronous in-flight state before the awaited call settles.
    expect(btn.disabled).toBe(true);
    expect(btn.textContent).toBe("Starting…");
    await flush();
    const err = root.querySelector(".onboard-start-err") as HTMLElement;
    expect(err.hidden).toBe(false);
    expect(err.textContent).toContain("port 8788 busy");
    // Re-armed so the user can retry.
    expect(btn.disabled).toBe(false);
    expect(btn.textContent).toBe("Start capture service");
  });

  it("shows a running state on success", async () => {
    const client = fakeClient({
      canControlProxy: () => true,
      proxyStart: async () => ({ running: true, port: 8788, url: "http://127.0.0.1:8788" }),
    });
    const root = document.createElement("div");
    document.body.appendChild(root);
    await renderOnboarding(root, client);
    const btn = btnByText(root, "Start capture service")!;
    btn.click();
    await flush();
    expect(btn.textContent).toBe("Capture service running ✓");
    const err = root.querySelector(".onboard-start-err") as HTMLElement;
    expect(err.hidden).toBe(true);
  });
});

describe("onboarding: connect is optional", () => {
  it("leads with zero-setup capture and tucks connect into an optional disclosure", async () => {
    const root = document.createElement("div");
    await renderOnboarding(root, fakeClient({ canControlProxy: () => true }));
    // Leads with the zero-setup message, not a required connect step.
    expect(root.textContent).toContain("Ready to capture");
    // The connect snippet + capture-service button live inside an OPTIONAL disclosure.
    const details = root.querySelector("details.onboard-optional") as HTMLDetailsElement;
    expect(details).toBeTruthy();
    expect(details.querySelector("summary")?.textContent?.toLowerCase()).toContain("optional");
    expect(root.querySelector("select")).toBeNull(); // no provider wall during first-run
    expect(details.querySelector("a")?.getAttribute("href")).toContain("sheet=capture");
    const startBtn = btnByText(root, "Start capture service");
    expect(startBtn && details.contains(startBtn)).toBe(true);
    // The primary action is simply moving on to see spend — connect is not on the required path.
    expect(btnByText(root, "Next → See spend")).toBeTruthy();
  });
});

describe("onboarding: heavy existing user", () => {
  it("advances See-spend on historical sessions when today's spend is 0, and never seeds demo over real data", async () => {
    let seeded = false;
    const client = fakeClient({
      canControlProxy: () => true,
      // Prior-day sessions only: today is $0, but catch-up already ingested real history.
      today: async () => ({ total_micros: 0, pricing_version: "x", effective_date: "2026-06-01" }),
      sessions: async () => ({
        rows: [
          {
            session: "old-1",
            runs: 1,
            steps: 5,
            tokens: 900,
            micros: 4_000_000,
            tools: 0,
            agents: 1,
            micros_per_step: 800_000,
            input_curve: [100, 200],
            cache_erosion_turn: 3,
          },
        ],
        total_micros: 4_000_000,
        pricing_version: "x",
        estimated: true,
      }),
      listRuns: async () => ["old-run-1"],
      seedDemo: async () => {
        seeded = true;
        return "demo";
      },
    });
    const root = document.createElement("div");
    document.body.appendChild(root);
    await renderOnboarding(root, client);
    btnByText(root, "Next → See spend")!.click(); // enter the profiling step (runs check() immediately)
    await flush();

    // The step advances without waiting for a NEW turn today.
    const seeBtn = btnByText(root, "Open Run Profile →")!;
    expect(seeBtn).toBeTruthy();
    expect(seeBtn.disabled).toBe(false);
    const status = root.querySelector(".onboard-wait p") as HTMLElement;
    expect(status.textContent).toContain("Found your sessions");

    // Finishing must NOT seed demo data over the user's real (prior-day) history.
    seeBtn.click();
    await flush();
    expect(seeded).toBe(false);
  });
});

describe("onboarding: profile-poll troubleshooting", () => {
  it("reveals a troubleshooting link if no event lands within the window", async () => {
    vi.useFakeTimers();
    const client = fakeClient({ canControlProxy: () => true }); // today() → $0, so the poll keeps waiting
    const root = document.createElement("div");
    document.body.appendChild(root);
    await renderOnboarding(root, client);
    btnByText(root, "Next → See spend")!.click(); // → the profiling step, which starts the poll
    const ts = root.querySelector(".onboard-troubleshoot") as HTMLElement;
    expect(ts).toBeTruthy();
    expect(ts.hidden).toBe(true); // not shown while we're still within the window
    vi.advanceTimersByTime(30_000);
    expect(ts.hidden).toBe(false);
    expect(ts.querySelector("a")?.getAttribute("href")).toContain("sheet=capture");
  });

  it("cancels the troubleshoot timer when leaving the profile step (no stale reveal)", async () => {
    vi.useFakeTimers();
    const client = fakeClient({ canControlProxy: () => true });
    const root = document.createElement("div");
    document.body.appendChild(root);
    await renderOnboarding(root, client);
    btnByText(root, "Next → See spend")!.click();
    const ts = root.querySelector(".onboard-troubleshoot") as HTMLElement;
    expect(ts.hidden).toBe(true);
    btnByText(root, "← Back")!.click(); // leave the profile step before the timer fires
    vi.advanceTimersByTime(30_000);
    // The timer was cancelled by render(), so the (now-detached) element never flips visible.
    expect(ts.hidden).toBe(true);
  });
});
