// Capture utility sheet: one detected/recommended path first,
// explicit privacy/local boundaries, actionable self-checks, and progressive advanced setup.

import { describe, it, expect } from "vitest";
import { renderCapture } from "../src/screens/capture.js";
import { fakeClient } from "./fakeClient.js";
import type { TareClient } from "../src/client.js";

const flush = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 0));

function flowing(overrides: Partial<TareClient> = {}): TareClient {
  return fakeClient({
    coverage: async () => ({
      status: "amber",
      has_proxy: false,
      has_otel: true,
      blind_sources: [],
      sources: [{ source: "otel-event", steps: 12, last_day: "2026-07-16", heartbeat: true }],
    }),
    sessionsLive: async () => [
      {
        session: "session-a",
        source: "claude-code",
        state: "working",
        last_seen_age_s: 3,
        events: 4,
        last_model: "claude-sonnet-4-6",
        micros: 1_200_000,
      },
    ],
    otlpStatus: async () => ({
      listening: true,
      port: 4318,
      events: 12,
      last_event_unix: 1,
      age_seconds: 3,
      hooks_seen: 2,
      hooks: [{ event: "SessionStart", count: 2, age_seconds: 3 }],
    }),
    config: async () => ({
      budget: {},
      privacy: { profile: "strict_counts" },
      providers: {},
      proxy: {},
    }),
    pricing: async () => ({
      version: "2026.07.10",
      effective_date: "2026-07-10",
      note: "Bundled locally.",
    }),
    ...overrides,
  });
}

describe("Capture utility sheet", () => {
  it("leads with detected state, agent, privacy boundary, and one recommendation", async () => {
    const root = document.createElement("div");
    await renderCapture(root, flowing());

    expect(root.dataset.captureState).toBe("flowing");
    expect(root.textContent).toContain("Current capture");
    expect(root.textContent).toContain("Captured cost events are flowing");
    expect(root.textContent).toContain("Claude Code");
    expect(root.textContent).toContain("Strict counts");
    expect(root.textContent).toContain("No prompt or response text is stored");
    expect(root.textContent).toContain("does not prove complete capture");
    expect(root.textContent).toContain("No setup change is required");
    expect(root.textContent).toContain("Pricing 2026.07.10");

    // Provider-specific configuration is lazy and collapsed: no provider wall on first view.
    const advanced = root.querySelector("details.capture-advanced") as HTMLDetailsElement;
    expect(advanced.open).toBe(false);
    expect(advanced.querySelector(".capture-advanced-body")?.children).toHaveLength(0);
    expect(root.querySelector("select.provider-picker")).toBeNull();
  });

  it("runs an actionable self-check and never upgrades channel health to completeness", async () => {
    const root = document.createElement("div");
    await renderCapture(root, flowing());
    const button = Array.from(root.querySelectorAll("button")).find(
      (candidate) => candidate.textContent === "Run self-check"
    ) as HTMLButtonElement;
    button.click();
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe("Checking…");
    await flush();

    const checks = root.querySelector(".capture-checks") as HTMLElement;
    expect(checks.textContent).toContain("Local store");
    expect(checks.textContent).toContain("Capture service");
    expect(checks.textContent).toContain("Event flow");
    expect(checks.textContent).toContain("Agent wiring");
    expect(checks.textContent).toContain("Pricing");
    expect(checks.querySelectorAll("[data-check-status='ok']").length).toBeGreaterThanOrEqual(4);
    expect(checks.textContent).not.toMatch(/100%|complete capture/i);
  });

  it("states blind and unavailable states honestly with a concrete remedy", async () => {
    const blindRoot = document.createElement("div");
    await renderCapture(
      blindRoot,
      flowing({
        coverage: async () => ({
          status: "red",
          has_proxy: false,
          has_otel: false,
          blind_sources: ["codex"],
          sources: [{ source: "codex", steps: 0, last_day: "", heartbeat: true }],
        }),
        sessionsLive: async () => [],
        otlpStatus: async () => ({
          listening: true,
          port: 4318,
          events: 0,
          last_event_unix: 0,
          age_seconds: null,
        }),
      })
    );
    expect(blindRoot.dataset.captureState).toBe("blind");
    expect(blindRoot.textContent).toContain("activity is visible, but no cost events are captured");
    expect(blindRoot.textContent).toContain("restart the agent");
    expect(blindRoot.textContent).toContain("OTEL_EXPORTER_OTLP_ENDPOINT");

    const unavailable = document.createElement("div");
    const down = async (): Promise<never> => {
      throw new Error("local service unavailable");
    };
    await renderCapture(
      unavailable,
      fakeClient({ coverage: down, sessionsLive: down, otlpStatus: down, proxyStatus: down, config: down, pricing: down })
    );
    expect(unavailable.dataset.captureState).toBe("unknown");
    expect(unavailable.textContent).toContain("Capture state unavailable");
    expect(unavailable.textContent).toContain("not evidence that capture is healthy");
    expect(unavailable.textContent).toContain("Restart the local capture service");
  });

  it("loads provider-specific OTel/proxy setup only when Advanced is opened", async () => {
    const root = document.createElement("div");
    await renderCapture(root, flowing());
    const advanced = root.querySelector("details.capture-advanced") as HTMLDetailsElement;
    advanced.open = true;
    advanced.dispatchEvent(new Event("toggle"));
    for (let attempt = 0; attempt < 20 && !advanced.querySelector("select.provider-picker"); attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    expect(advanced.querySelector("select.provider-picker")).toBeTruthy();
    expect(advanced.textContent).toContain("Advanced: deep attribution via the proxy channel");
  });

  it("surfaces a start failure with a retryable remedy", async () => {
    const root = document.createElement("div");
    await renderCapture(
      root,
      flowing({
        canControlProxy: () => true,
        proxyStart: async () => {
          throw new Error("port 8788 busy");
        },
      })
    );
    const start = Array.from(root.querySelectorAll("button")).find(
      (candidate) => candidate.textContent === "Restart capture service"
    ) as HTMLButtonElement;
    start.click();
    await flush();
    expect(root.querySelector(".capture-start-error")?.textContent).toContain("port 8788 busy");
    expect(root.querySelector(".capture-start-error")?.textContent).toContain("close the other process");
    expect(start.disabled).toBe(false);
  });
});
