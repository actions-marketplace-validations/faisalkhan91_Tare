// Focused Capture utility sheet. The first view answers whether local
// data is flowing, which agent/source is visible, what the privacy boundary retains, and the ONE
// recommended next step. Provider-specific OTel/proxy recipes remain lazy behind Advanced.

import { el } from "../ui/el.js";
import { icon } from "../ui/icon.js";
import { humanizeKey } from "../ui/format.js";
import { otelEnvShell } from "../ui/connectEnv.js";
import { privacyProfileDescription, privacyProfileInfo } from "../ui/privacyProfiles.js";
import type {
  Coverage,
  OtlpStatus,
  PricingInfo,
  ProxyStatus,
  SessionLive,
  TareClient,
  TareConfigDto,
} from "../client.js";

interface Result<T> {
  value: T | null;
  error?: unknown;
}

interface CaptureSnapshot {
  coverage: Result<Coverage>;
  agents: Result<SessionLive[]>;
  otlp: Result<OtlpStatus>;
  proxy: Result<ProxyStatus>;
  config: Result<TareConfigDto>;
  pricing: Result<PricingInfo>;
}

type CaptureState = "flowing" | "blind" | "empty" | "unknown";
type CheckState = "ok" | "warn" | "bad";

function settled<T>(result: PromiseSettledResult<T>): Result<T> {
  return result.status === "fulfilled"
    ? { value: result.value }
    : { value: null, error: result.reason };
}

async function captureSnapshot(client: TareClient): Promise<CaptureSnapshot> {
  const [coverage, agents, otlp, proxy, config, pricing] = await Promise.allSettled([
    client.coverage(),
    client.sessionsLive(),
    client.otlpStatus(),
    client.proxyStatus(),
    client.config(),
    client.pricing(),
  ] as const);
  return {
    coverage: settled(coverage),
    agents: settled(agents),
    otlp: settled(otlp),
    proxy: settled(proxy),
    config: settled(config),
    pricing: settled(pricing),
  };
}

function captureState(snapshot: CaptureSnapshot): CaptureState {
  const coverage = snapshot.coverage.value;
  if (!coverage) return "unknown";
  if (coverage.status === "red") return "blind";
  if (coverage.status === "none") return "empty";
  return "flowing";
}

function sourceLabel(value: string): string {
  const normalized = value.toLowerCase();
  if (normalized.includes("claude")) return "Claude Code";
  if (normalized.includes("codex")) return "Codex";
  if (normalized.includes("gemini")) return "Gemini CLI";
  if (normalized === "proxy") return "Proxy channel";
  if (normalized.startsWith("otel")) return "OTel channel";
  if (normalized.includes("jsonl")) return "Local JSONL";
  return humanizeKey(value);
}

function detectedLabels(snapshot: CaptureSnapshot): string[] {
  const labels = new Set<string>();
  for (const agent of snapshot.agents.value ?? []) labels.add(sourceLabel(agent.source));
  for (const source of snapshot.coverage.value?.sources ?? []) labels.add(sourceLabel(source.source));
  return [...labels];
}

function copyButton(text: string): HTMLButtonElement {
  const button = el("button", { class: "btn ghost", type: "button", text: "Copy configuration" }) as HTMLButtonElement;
  button.addEventListener("click", () => {
    try {
      void navigator.clipboard?.writeText(text);
    } catch {
      /* The configuration remains selectable when clipboard access is unavailable. */
    }
    button.textContent = "Copied ✓";
  });
  return button;
}

function stateSection(snapshot: CaptureSnapshot): HTMLElement {
  const state = captureState(snapshot);
  const coverage = snapshot.coverage.value;
  const labels = detectedLabels(snapshot);
  let headline: string;
  let detail: string;
  let tone: string;
  if (state === "flowing") {
    const channels = [coverage?.has_otel ? "OTel" : "", coverage?.has_proxy ? "proxy" : ""]
      .filter(Boolean)
      .join(" + ");
    headline = `Captured cost events are flowing${channels ? ` through ${channels}` : ""}.`;
    detail = "Channel health describes the local data Tare can see; it does not prove complete capture or account for usage outside these channels.";
    tone = "cost-ok";
  } else if (state === "blind") {
    headline = "Agent activity is visible, but no cost events are captured.";
    detail = `${coverage?.blind_sources.map(sourceLabel).join(", ") || "A local agent"} is sending heartbeats without priced usage. That spend is missing, not $0.`;
    tone = "cost-high";
  } else if (state === "empty") {
    headline = "No captured cost events yet.";
    detail = "This is an empty local history, not evidence of zero usage. Use an agent once, then run the self-check below.";
    tone = "cost-warn";
  } else {
    headline = "Capture state unavailable.";
    detail = "The local status checks could not be read. Unknown is not evidence that capture is healthy or that usage is zero.";
    tone = "cost-high";
  }

  return el("section", { class: "section capture-current" }, [
    el("h2", { text: "Current capture" }),
    el("p", { class: `capture-headline ${tone}` }, [
      icon(state === "flowing" ? "dot" : "warning", {
        size: 13,
        label: state === "flowing" ? undefined : "Warning",
      }),
      ` ${headline}`,
    ]),
    el("p", { class: "caption", text: detail }),
    el("p", {
      class: "caption capture-detected",
      text: labels.length > 0
        ? `Detected agents and sources: ${labels.join(", ")}.`
        : "No agent identity or capture source is visible yet.",
    }),
  ]);
}

function privacySection(snapshot: CaptureSnapshot): HTMLElement {
  const profile = snapshot.config.value?.privacy.profile ?? "strict_counts";
  const info = privacyProfileInfo(profile);
  return el("section", { class: "section capture-privacy" }, [
    el("h2", { text: "Privacy boundary" }),
    snapshot.config.value
      ? el("p", { class: "capture-keyline" }, [
          el("strong", { text: info?.label ?? humanizeKey(profile) }),
          ` — ${privacyProfileDescription(profile) || "This newer privacy profile is active; inspect Settings for its exact retention policy."}`,
        ])
      : el("p", {
          class: "cost-warn",
          text: "Privacy configuration unavailable. Capture remains local, but the active retention profile could not be confirmed.",
        }),
    el("p", {
      class: "caption sub",
      text: "Capture, status checks, configuration, and pricing stay on this machine. This sheet makes no network request and needs no account.",
    }),
  ]);
}

function recommendationSection(snapshot: CaptureSnapshot, client: TareClient): HTMLElement {
  const state = captureState(snapshot);
  const otlpPort = snapshot.otlp.value?.port || snapshot.config.value?.proxy.otlp_port || 4318;
  const snippet = otelEnvShell(`http://127.0.0.1:${otlpPort}`);
  const section = el("section", { class: "section capture-recommendation" }, [
    el("h2", { text: "Recommended next step" }),
  ]);
  if (state === "flowing") {
    section.appendChild(
      el("p", {
        text: "No setup change is required. Keep using the detected local capture path, then run the self-check after any agent or configuration change.",
      })
    );
  } else if (state === "blind") {
    section.appendChild(
      el("p", {
        text: "Use the OTel path already listening on this machine: copy this block, restart the agent, produce one model response, then run the self-check.",
      })
    );
  } else if (state === "empty") {
    section.appendChild(
      el("p", {
        text: "Claude Code needs no setup: use it once and Tare will read its local JSONL history. For live per-request capture, use this one additive OTel configuration and restart the agent.",
      })
    );
  } else {
    section.appendChild(
      el("p", {
        text: "Restart the local capture service, then run the self-check. If it remains unavailable, verify local database permissions and that ports 8788 and 4318 are free.",
      })
    );
  }
  if (state !== "flowing") {
    section.append(
      el("pre", {
        class: "explain capture-recommended-config",
        tabindex: "0",
        "aria-label": "Recommended OpenTelemetry configuration",
        text: snippet,
      }),
      copyButton(snippet)
    );
  }

  if (client.canControlProxy?.()) {
    const serviceLive = snapshot.otlp.value?.listening || snapshot.proxy.value?.running;
    const error = el("p", { class: "error capture-start-error" });
    error.hidden = true;
    const button = el("button", {
      class: "btn",
      type: "button",
      text: serviceLive ? "Restart capture service" : "Start capture service",
    }) as HTMLButtonElement;
    button.addEventListener("click", async () => {
      button.disabled = true;
      button.textContent = "Starting…";
      error.hidden = true;
      try {
        await client.proxyStart();
        button.textContent = "Capture service running ✓";
      } catch (cause) {
        button.disabled = false;
        button.textContent = serviceLive ? "Restart capture service" : "Start capture service";
        const message = cause instanceof Error ? cause.message : String(cause);
        error.textContent = `Couldn't start the local capture service: ${message}. If a port is busy, close the other process or change the configured ports, then retry.`;
        error.hidden = false;
      }
    });
    section.append(button, error);
  }
  return section;
}

function checkRow(status: CheckState, name: string, message: string, fix?: string): HTMLElement {
  return el("li", { class: "capture-check", "data-check-status": status }, [
    el("span", { class: `capture-check-mark check-${status}`, text: status === "ok" ? "✓" : status === "warn" ? "⚠" : "✗" }),
    el("span", { class: "capture-check-copy" }, [
      el("strong", { text: name }),
      el("span", { text: ` — ${message}` }),
      fix ? el("span", { class: "capture-check-fix", text: ` Remedy: ${fix}` }) : false,
    ]),
  ]);
}

function renderChecks(host: HTMLElement, snapshot: CaptureSnapshot): void {
  const coverage = snapshot.coverage.value;
  const otlp = snapshot.otlp.value;
  const stepCount = coverage?.sources.reduce((sum, source) => sum + source.steps, 0) ?? 0;
  const agentLabels = detectedLabels(snapshot);
  const rows: HTMLElement[] = [];
  rows.push(
    snapshot.coverage.value
      ? checkRow("ok", "Local store", "coverage metadata is readable locally")
      : checkRow("bad", "Local store", "coverage metadata could not be read", "restart the local service and verify database permissions")
  );
  rows.push(
    otlp?.listening
      ? checkRow("ok", "Capture service", `OTel receiver is listening on :${otlp.port}`)
      : checkRow("bad", "Capture service", "OTel receiver is not confirmed listening", "start or restart the local capture service")
  );
  rows.push(
    stepCount > 0 || (otlp?.events ?? 0) > 0
      ? checkRow("ok", "Event flow", `${Math.max(stepCount, otlp?.events ?? 0)} captured event${Math.max(stepCount, otlp?.events ?? 0) === 1 ? "" : "s"} observed`)
      : coverage?.status === "red"
        ? checkRow("bad", "Event flow", "agent activity has no captured cost events", "copy the recommended OTel block, restart the agent, and produce one response")
        : checkRow("warn", "Event flow", "no captured event has arrived yet", "use an agent once, then rerun this self-check")
  );
  rows.push(
    agentLabels.length > 0 || (otlp?.hooks_seen ?? 0) > 0
      ? checkRow("ok", "Agent wiring", agentLabels.length > 0 ? agentLabels.join(", ") : "lifecycle hooks are firing")
      : checkRow("warn", "Agent wiring", "no agent or source is detected", "run `tare detect --wire`, restart the agent, then rerun this check")
  );
  rows.push(
    snapshot.config.value
      ? checkRow("ok", "Privacy", `${privacyProfileInfo(snapshot.config.value.privacy.profile ?? "strict_counts")?.label ?? "Local profile"} is active`)
      : checkRow("warn", "Privacy", "active profile could not be confirmed", "open Settings → Privacy & Capture and verify the local config")
  );
  rows.push(
    snapshot.pricing.value
      ? checkRow("ok", "Pricing", `bundled edition ${snapshot.pricing.value.version} is readable offline`)
      : checkRow("bad", "Pricing", "bundled pricing could not be read", "restart Tare or supply a valid local pricing file")
  );
  host.replaceChildren(el("ul", { class: "capture-check-list" }, rows));
}

function selfCheckSection(client: TareClient): HTMLElement {
  const result = el("div", { class: "capture-checks", "aria-live": "polite" });
  const button = el("button", { class: "btn primary", type: "button", text: "Run self-check" }) as HTMLButtonElement;
  button.addEventListener("click", async () => {
    button.disabled = true;
    button.textContent = "Checking…";
    result.replaceChildren(el("p", { class: "skeleton", text: "Checking local capture…" }));
    const snapshot = await captureSnapshot(client);
    renderChecks(result, snapshot);
    button.disabled = false;
    button.textContent = "Run self-check again";
  });
  return el("section", { class: "section capture-doctor" }, [
    el("h2", { text: "Self-check" }),
    el("p", {
      class: "caption",
      text: "Re-check the local store, receiver, event flow, detected agent wiring, privacy profile, and bundled pricing. Every warning includes its next remedy.",
    }),
    button,
    result,
  ]);
}

function advancedSection(client: TareClient): HTMLElement {
  const body = el("div", { class: "capture-advanced-body" });
  const details = el("details", { class: "capture-advanced" }, [
    el("summary", { text: "Advanced OTel, proxy, and provider-specific setup" }),
    body,
  ]) as HTMLDetailsElement;
  let loaded = false;
  details.addEventListener("toggle", () => {
    if (!details.open || loaded) return;
    loaded = true;
    body.replaceChildren(el("p", { class: "skeleton", text: "Loading advanced capture setup…" }));
    void import("./connect.js")
      .then((module) => module.renderConnect(body, client))
      .catch((error) => {
        body.replaceChildren(
          el("p", {
            class: "error",
            text: `Couldn't load advanced capture setup: ${error instanceof Error ? error.message : String(error)}. Close and reopen the sheet to retry.`,
          })
        );
      });
  });
  return details;
}

export async function renderCapture(root: HTMLElement, client: TareClient): Promise<void> {
  root.replaceChildren(el("p", { class: "skeleton", text: "Loading local capture state…" }));
  const snapshot = await captureSnapshot(client);
  root.dataset.captureState = captureState(snapshot);
  const pricing = snapshot.pricing.value;
  root.replaceChildren(
    stateSection(snapshot),
    privacySection(snapshot),
    recommendationSection(snapshot, client),
    selfCheckSection(client),
    el("p", {
      class: "caption sub capture-pricing-footnote",
      text: pricing
        ? `Pricing ${pricing.version}, effective ${pricing.effective_date}, is bundled locally. Dollar figures remain estimates, never invoices.`
        : "Pricing freshness unavailable. Dollar estimates cannot be verified until the local price book loads.",
    }),
    advancedSection(client)
  );
}
