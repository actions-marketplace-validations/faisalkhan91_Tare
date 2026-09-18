import { describe, it, expect } from "vitest";
import {
  connectEnvFor,
  connectEnvShellFor,
  otelEnv,
  otelEnvShell,
  PROVIDER_PRESETS,
  providerPreset,
} from "../src/ui/connectEnv.js";
import { renderConnect } from "../src/screens/connect.js";
import { fakeClient } from "./fakeClient.js";

describe("pricingFreshnessLine", () => {
  it("states the table version + how many of the provider's models are priced", async () => {
    const { pricingFreshnessLine } = await import("../src/ui/connectEnv.js");
    const counts = { anthropic: 7, azure_openai: 3 };
    const a = pricingFreshnessLine("2026.06.01", "2026-06-01", "anthropic", counts);
    expect(a).toContain("2026.06.01");
    expect(a).toContain("7 models priced");
    // azure picker id maps to the azure_openai pricing key.
    expect(pricingFreshnessLine("2026.06.01", "2026-06-01", "azure", counts)).toContain("3 models priced");
    // OpenAI-compatible / usage-only providers say so honestly (no bundled prices).
    const o = pricingFreshnessLine("2026.06.01", "2026-06-01", "openrouter", counts);
    expect(o).toContain("usage-only by default");
    expect(o).toContain("usage-only");
  });
});

describe("captureRecommendation", () => {
  it("recommends OTel for Claude Code / Codex and the proxy for proxiable SDKs", async () => {
    const { captureRecommendation } = await import("../src/ui/connectEnv.js");
    expect(captureRecommendation("anthropic").mode).toBe("otel");
    expect(captureRecommendation("bedrock").mode).toBe("otel");
    expect(captureRecommendation("openai").mode).toBe("proxy");
    expect(captureRecommendation("gemini").mode).toBe("proxy");
    expect(captureRecommendation("other").mode).toBe("proxy");
    // Each carries a one-line why.
    expect(captureRecommendation("anthropic").why.length).toBeGreaterThan(10);
  });

  it("renders a leading recommendation banner that reflects the picked provider", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient());
    const reco = root.querySelector(".capture-reco") as HTMLElement;
    expect(reco).toBeTruthy();
    expect(reco.querySelector(".reco-tag")?.textContent).toBe("Recommended");
    // Default provider (anthropic) -> OTel channel recommendation text.
    expect(reco.textContent).toContain("OTel channel");
    // Switching the provider picker to OpenAI updates it to the proxy-channel recommendation.
    const sel = root.querySelector("select.provider-picker") as HTMLSelectElement;
    sel.value = "openai";
    sel.dispatchEvent(new Event("change"));
    expect(reco.textContent).toContain("proxy channel");
    localStorage.removeItem("tare-provider"); // don't leak the pick into later tests
  });
});

describe("agent recipes", () => {
  it("maps OTel vs proxy agents to the right snippet", async () => {
    const { agentRecipe } = await import("../src/ui/connectEnv.js");
    // Gemini CLI → OTel: an OTLP env block pointing at the receiver.
    const gem = agentRecipe("gemini-cli");
    expect(gem.mode).toBe("otel");
    expect(gem.render("http://127.0.0.1:4318", "http://127.0.0.1:8788")).toContain("OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318");
    // Aider → proxy: an OpenAI base-URL env pointing at the proxy.
    const aider = agentRecipe("aider");
    expect(aider.mode).toBe("proxy");
    expect(aider.render("http://127.0.0.1:4318", "http://127.0.0.1:8788")).toBe("export OPENAI_API_BASE=http://127.0.0.1:8788/v1");
    // Unknown id falls back to the first recipe (Claude Code).
    expect(agentRecipe("nope").id).toBe("claude-code");
  });

  it("renders the Other-agents recipe section, switching snippet on select", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient());
    const section = Array.from(root.querySelectorAll("section")).find((s) => s.textContent?.includes("Other agents")) as HTMLElement;
    expect(section).toBeTruthy();
    const sel = section.querySelector("select") as HTMLSelectElement;
    sel.value = "aider";
    sel.dispatchEvent(new Event("change"));
    expect(section.querySelector("pre")?.textContent).toContain("OPENAI_API_BASE");
    expect(section.textContent).toContain("Proxy channel");
  });
});

describe("connectEnvFor (provider presets)", () => {
  it("scopes the env to a single provider, base URLs only — never keys", () => {
    const anthropic = Object.fromEntries(connectEnvFor("anthropic", "http://127.0.0.1:8788/"));
    expect(anthropic).toEqual({ ANTHROPIC_BASE_URL: "http://127.0.0.1:8788" });

    const openai = Object.fromEntries(connectEnvFor("openai", "http://127.0.0.1:8788"));
    expect(openai).toEqual({
      OPENAI_BASE_URL: "http://127.0.0.1:8788/v1",
      OPENAI_API_BASE: "http://127.0.0.1:8788/v1",
    });

    // OpenAI-compatible providers reuse the OpenAI rows.
    for (const id of ["openrouter", "local", "other"] as const) {
      expect(connectEnvFor(id, "http://h:1").map(([k]) => k)).toEqual(["OPENAI_BASE_URL", "OPENAI_API_BASE"]);
    }

    // No preset emits an API key.
    for (const p of PROVIDER_PRESETS) {
      expect(connectEnvFor(p.id, "http://h:1").some(([k]) => /KEY/i.test(k))).toBe(false);
    }

    // Bedrock has no portable base-URL override.
    expect(connectEnvFor("bedrock", "http://h:1")).toEqual([]);
  });

  it("renders a copyable shell block and resolves presets with an Anthropic fallback", () => {
    expect(connectEnvShellFor("gemini", "http://h:1")).toBe(
      "export GEMINI_BASE_URL=http://h:1\nexport GOOGLE_GEMINI_BASE_URL=http://h:1"
    );
    expect(providerPreset("nonexistent").id).toBe("anthropic");
  });
});

describe("capture environment", () => {
  it("builds the out-of-band OTel capture env (additive, points at the receiver)", () => {
    const env = Object.fromEntries(otelEnv("http://127.0.0.1:4318/"));
    expect(env.CLAUDE_CODE_ENABLE_TELEMETRY).toBe("1");
    expect(env.OTEL_METRICS_EXPORTER).toBe("otlp");
    expect(env.OTEL_LOGS_EXPORTER).toBe("otlp");
    expect(env.OTEL_EXPORTER_OTLP_PROTOCOL).toBe("http/json");
    expect(env.OTEL_EXPORTER_OTLP_ENDPOINT).toBe("http://127.0.0.1:4318");
    // Orthogonal to ANTHROPIC_BASE_URL — never fights the existing proxy slot.
    expect(env.ANTHROPIC_BASE_URL).toBeUndefined();
    expect(otelEnvShell("http://h:4318")).toContain("export OTEL_EXPORTER_OTLP_ENDPOINT=http://h:4318");
  });
});

describe("Connect screen", () => {
  it("shows copyable env rows and no proxy control on the browser transport", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient({ canControlProxy: () => false }));
    // The provider picker defaults to Anthropic, so the proxy block shows ANTHROPIC_BASE_URL only.
    expect(root.textContent).toContain("ANTHROPIC_BASE_URL");
    expect(
      Array.from(root.querySelectorAll("pre")).some((p) =>
        (p.textContent ?? "").includes("export ANTHROPIC_BASE_URL")
      )
    ).toBe(true);
    // No "Start proxy channel" control in the browser.
    expect(Array.from(root.querySelectorAll("button")).some((b) => /proxy/i.test(b.textContent ?? ""))).toBe(false);
  });

  it("surfaces a pricing-freshness line that reflects the chosen provider", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => false,
        pricing: async () => ({ version: "2026.06.01", effective_date: "2026-06-01", note: null, models_by_provider: { anthropic: 7 } }),
      })
    );
    const line = root.querySelector(".pricing-freshness");
    expect(line).toBeTruthy();
    // Default provider is Anthropic -> 7 priced models.
    expect(line?.textContent).toContain("2026.06.01");
    expect(line?.textContent).toContain("7 models priced");
    localStorage.removeItem("tare-provider");
  });

  it("scopes the proxy env block to the provider chosen in the picker", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient({ canControlProxy: () => false }));
    const picker = Array.from(root.querySelectorAll("select")).find((s) =>
      Array.from(s.options).some((o) => o.value === "openai")
    ) as HTMLSelectElement;
    expect(picker).toBeTruthy();
    // Default = Anthropic: no OpenAI rows yet.
    expect(root.textContent).not.toContain("OPENAI_BASE_URL");
    // Pick OpenAI -> only OpenAI's base-URL rows render (Anthropic row gone from the proxy block).
    picker.value = "openai";
    picker.dispatchEvent(new Event("change"));
    expect(
      Array.from(root.querySelectorAll("pre")).some((p) => (p.textContent ?? "").includes("export OPENAI_BASE_URL"))
    ).toBe(true);
    // Pick Bedrock -> no base-URL override; the block explains the OTel path instead.
    picker.value = "bedrock";
    picker.dispatchEvent(new Event("change"));
    expect(root.textContent).toContain("No base-URL override");
    localStorage.removeItem("tare-provider"); // don't leak the choice into other tests
  });

  it("shows the proxy Start/Stop control on the desktop transport", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => true,
        proxyStatus: async () => ({ running: false, port: 0, url: "" }),
      })
    );
    expect(
      Array.from(root.querySelectorAll("button")).some((b) => b.textContent === "Start proxy channel")
    ).toBe(true);
  });

  it("shows the running proxy with its port (regression: port was dropped)", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => true,
        proxyStatus: async () => ({ running: true, port: 8788, url: "http://127.0.0.1:8788" }),
      })
    );
    expect(root.textContent).toContain("running on :8788");
  });

  it("shows live OTLP receiver status when the client exposes it", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        otlpStatus: async () => ({
          listening: true,
          port: 4318,
          events: 5,
          last_event_unix: 1,
          age_seconds: 12,
        }),
      })
    );
    expect(root.textContent).toContain("OTel channel listening on :4318");
    expect(root.textContent).toContain("5 events captured");
    expect(root.textContent).toContain("last 12s ago");
    // No hooks reported (events flowing) → a fixable warning leading with impact + a "Fix this"
    // disclosure holding the remedy, not a jargon wall.
    expect(root.textContent).toContain("Session start and end times may be approximate");
    const fix = Array.from(root.querySelectorAll("details.hook-fix summary")).find((s) => s.textContent === "Fix this");
    expect(fix).toBeTruthy();
    expect(root.textContent).toContain("tare connect"); // remedy still present, inside the disclosure
  });

  it("reports healthy lifecycle hooks when they are firing", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        otlpStatus: async () => ({
          listening: true,
          port: 4318,
          events: 5,
          last_event_unix: 1,
          age_seconds: 12,
          hooks_seen: 3,
          hooks: [{ event: "SessionStart", count: 3, age_seconds: 4 }],
        }),
      })
    );
    expect(root.textContent).toContain("Lifecycle hooks firing");
    expect(root.textContent).not.toContain("No lifecycle hooks received");
  });

  it("documents the out-of-band OTLP capture path pointed at the OTel channel, listed first", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient({ canControlProxy: () => false }));
    // The OTel path is the primary "Connect an agent" section — now framed as an OPTIONAL
    // richer-real-time upgrade, since JSONL capture works with zero setup.
    expect(root.textContent).toContain("Connect an agent (optional");
    expect(root.textContent).toContain("CLAUDE_CODE_ENABLE_TELEMETRY");
    // points at the receiver port (4318), derived from the proxy origin host
    expect(root.textContent).toContain(":4318");
    // pasteable block present
    expect(
      Array.from(root.querySelectorAll("pre")).some((p) =>
        (p.textContent ?? "").includes("OTEL_EXPORTER_OTLP_ENDPOINT")
      )
    ).toBe(true);
    // The OTel "Connect an agent" section is ordered BEFORE the advanced inline-proxy section.
    const headings = Array.from(root.querySelectorAll("h2")).map((h) => h.textContent ?? "");
    const otelIdx = headings.findIndex((h) => h.includes("Connect an agent"));
    const advIdx = headings.findIndex((h) => h.includes("Advanced"));
    expect(otelIdx).toBeGreaterThanOrEqual(0);
    expect(advIdx).toBeGreaterThan(otelIdx);
  });

  it("starting the proxy calls proxyStart and re-renders into the running state", async () => {
    const root = document.createElement("div");
    let running = false;
    let started = 0;
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => true,
        proxyStatus: async () => ({
          running,
          port: running ? 8788 : 0,
          url: running ? "http://127.0.0.1:8788" : "",
        }),
        proxyStart: async () => {
          started++;
          running = true;
          return { running: true, port: 8788, url: "http://127.0.0.1:8788" };
        },
      })
    );
    const btn = Array.from(root.querySelectorAll("button")).find(
      (b) => b.textContent === "Start proxy channel"
    ) as HTMLButtonElement;
    btn.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(started).toBe(1);
    expect(
      Array.from(root.querySelectorAll("button")).some((b) => b.textContent === "Stop proxy channel")
    ).toBe(true);
    expect(root.textContent).toContain("running on :8788");
  });

  it("copy buttons confirm the action by flipping their label", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient());
    const btn = Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "Copy");
    expect(btn).toBeTruthy();
    btn!.click();
    expect(btn!.textContent).toBe("Copied ✓");
  });

  it("renders a Local service card with proxy state pill, OTLP capture pill, and Claude Code cross-check", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => true,
        proxyStatus: async () => ({ running: true, port: 8788, url: "http://127.0.0.1:8788" }),
        otlpStatus: async () => ({ listening: true, port: 4318, events: 5, last_event_unix: 1, age_seconds: 12 }),
        vendorToday: async () => ({ cost_micros: 4_210_000, tokens: 12_345, day: "2026-06-29", available: true }),
      })
    );
    const card = Array.from(root.querySelectorAll("section")).find((s) =>
      s.querySelector("h2")?.textContent === "Capture service"
    ) as HTMLElement;
    expect(card).toBeTruthy();
    // Proxy state pill reads "Running" (sentence case); the HTTP port is shown.
    expect(card.querySelector(".state-pill")?.textContent).toContain("Running");
    expect(card.textContent).toContain("running on :8788");
    // The same capture-health pill Live paints, in its "capturing" state.
    expect(card.querySelector(".capture-pill.capture-ok")).toBeTruthy();
    expect(card.textContent).toContain("Last event 12s");
    // Claude Code cross-check row (available → shown, labelled as a cross-check).
    expect(card.textContent).toContain("$4.21");
    expect(card.textContent).toContain("cross-check");
  });

  it("Local service shows a stopped pill + Start proxy channel, and hides the vendor row when unavailable", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => true,
        proxyStatus: async () => ({ running: false, port: 0, url: "" }),
        otlpStatus: async () => ({ listening: false, port: 4318, events: 0, last_event_unix: 0, age_seconds: null }),
        vendorToday: async () => ({ cost_micros: 0, tokens: 0, day: "2026-06-29", available: false }),
      })
    );
    const card = Array.from(root.querySelectorAll("section")).find((s) =>
      s.querySelector("h2")?.textContent === "Capture service"
    ) as HTMLElement;
    expect(card.querySelector(".state-pill")?.textContent).toContain("Stopped"); // sentence case
    expect(Array.from(card.querySelectorAll("button")).some((b) => b.textContent === "Start proxy channel")).toBe(true);
    // Receiver not listening → the capture pill shows the error state, and no Claude Code cross-check row.
    expect(card.querySelector(".capture-pill.capture-error")).toBeTruthy();
    expect(card.textContent).not.toContain("cross-check");
  });

  it("Local service is desktop-only (absent on the browser transport)", async () => {
    const root = document.createElement("div");
    await renderConnect(root, fakeClient({ canControlProxy: () => false }));
    expect(Array.from(root.querySelectorAll("h2")).some((h) => h.textContent === "Capture service")).toBe(false);
  });

  it("surfaces an error and re-enables the button when proxyStart fails", async () => {
    const root = document.createElement("div");
    await renderConnect(
      root,
      fakeClient({
        canControlProxy: () => true,
        proxyStatus: async () => ({ running: false, port: 0, url: "" }),
        proxyStart: async () => {
          throw new Error("port in use");
        },
      })
    );
    const btn = Array.from(root.querySelectorAll("button")).find(
      (b) => b.textContent === "Start proxy channel"
    ) as HTMLButtonElement;
    btn.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(root.querySelector(".error")?.textContent).toContain("port in use");
    expect(btn.disabled).toBe(false);
  });
});
