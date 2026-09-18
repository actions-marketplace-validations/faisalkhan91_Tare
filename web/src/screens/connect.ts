// Connect: optional live-detail setup for the OTel and proxy channels. Shows the per-provider env
// to set (with copy), and on the desktop a Start/Stop control for the embedded capture service.
// In the browser, the serving origin is already the proxy.

import { el } from "../ui/el.js";
import { icon } from "../ui/icon.js";
import {
  connectEnvFor,
  connectEnvShellFor,
  pricingFreshnessLine,
  captureRecommendation,
  otelEnv,
  otelEnvShell,
  providerPreset,
  PROVIDER_PRESETS,
  AGENT_RECIPES,
  agentRecipe,
} from "../ui/connectEnv.js";
import type { ProviderId } from "../ui/connectEnv.js";
import { providerPref, setProviderPref } from "../ui/prefs.js";
import { statePill } from "../ui/statePill.js";
import { fmtUsd, fmtTokens, toDollarString } from "../ui/format.js";
import type { OtlpStatus, ProxyStatus, TareClient, VendorToday } from "../client.js";

const CAPTURE_QUIET_S = 540; // 9 min

function liveAge(s: number): string {
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  return `${Math.floor(s / 3600)}h ago`;
}

/// Paint the capture-status pill (relocated from the deleted legacy Live screen; Connect is its only
/// consumer). Without `mode` it keeps the legacy
/// OTLP-only pill (unreachable > quiet > capturing); with `mode`/`jsonl` it reflects the
/// always-on/app-open capture lanes honestly.
export function paintCapturePill(
  pill: HTMLElement,
  status: OtlpStatus | null,
  mode?: string,
  jsonl?: boolean
): void {
  let tone: "ok" | "warn" | "error";
  let text: string;
  const otlpLive = !!status && status.age_seconds !== null && status.age_seconds <= CAPTURE_QUIET_S;
  if (mode !== undefined) {
    if (mode === "off") {
      tone = "warn";
      text = "Capture paused";
    } else if (!status || !status.listening) {
      tone = "error";
      text = "Not capturing. Check setup";
    } else if (jsonl === false) {
      if (otlpLive) {
        tone = "ok";
        text = `Capturing via OTel · Live event ${liveAge(status.age_seconds as number)}`;
      } else {
        tone = "warn";
        text = "JSONL off · No capture. Connect OTel or re-enable JSONL";
      }
    } else {
      tone = "ok";
      const where = mode === "always_on" ? "always-on" : "while app open";
      text = otlpLive
        ? `Capturing (${where}) · Live event ${liveAge(status.age_seconds as number)}`
        : `Capturing (${where})`;
    }
  } else if (!status || !status.listening) {
    tone = "error";
    text = "OTel channel not reachable";
  } else if (status.age_seconds === null || status.age_seconds > CAPTURE_QUIET_S) {
    tone = "warn";
    text =
      status.age_seconds === null
        ? "No events yet. Is Connect active?"
        : `No events for ${liveAge(status.age_seconds)}. Is Connect active?`;
  } else {
    tone = "ok";
    text = `Capturing · Last event ${liveAge(status.age_seconds)}`;
  }
  pill.className = `capture-pill capture-${tone}`;
  pill.title = status
    ? `OTel channel on :${status.port} · ${status.events} event${status.events === 1 ? "" : "s"} captured`
    : "OTel channel unreachable";
  pill.replaceChildren(
    icon("dot", { size: 8, class: `capture-dot capture-${tone}` }),
    el("span", { class: "capture-text", text })
  );
}

function copy(text: string): void {
  try {
    void navigator?.clipboard?.writeText(text);
  } catch {
    /* clipboard unavailable; the text is visible to select manually */
  }
}

/// A copy button that confirms the action (briefly flips its label), so a click isn't silent.
function copyBtn(text: string, label = "Copy"): HTMLElement {
  const btn = el("button", { class: "btn", text: label }) as HTMLButtonElement;
  btn.addEventListener("click", () => {
    copy(text);
    btn.textContent = "Copied ✓";
    try {
      setTimeout(() => {
        btn.textContent = label;
      }, 1200);
    } catch {
      /* timers unavailable; leave the confirmation */
    }
  });
  return btn;
}

export async function renderConnect(root: HTMLElement, client: TareClient): Promise<void> {
  const canControl = client.canControlProxy?.() ?? false;
  let status: ProxyStatus = { running: false, port: 0, url: "" };
  try {
    status = await client.proxyStatus();
  } catch {
    /* keep default */
  }
  // Base URL: the running proxy's URL, else the browser origin, else a sensible default.
  const origin = (() => {
    try {
      return typeof location !== "undefined" ? location.origin : "";
    } catch {
      return "";
    }
  })();
  const baseUrl = status.url || origin || "http://127.0.0.1:8788";

  const frag = document.createDocumentFragment();

  // One product, not a bundle: every surface reads the same local store + attribution engine.
  frag.appendChild(
    el("p", {
      class: "caption",
      text: "Tare is one local store and one attribution engine, viewed many ways. The web viewer and desktop app monitor spend, the CLI supports terminal and CI workflows, and the MCP server lets your agent query its own spend. Once data is captured, every surface sees the same local record.",
    })
  );

  // Per-provider capture recommendation: decide the ONE mode that fits the picked
  // provider instead of presenting both with equal weight. Updated by the provider picker below;
  // both modes stay available (the non-recommended one is the "Advanced" section).
  const recoBanner = el("div", { class: "capture-reco", role: "note" });
  frag.appendChild(recoBanner);

  // ---- out-of-band capture (OTel channel) — the RECOMMENDED, default path ----
  // Shown FIRST: it's additive to existing network config, never in the request path, and works with any
  // ANTHROPIC_BASE_URL / corporate proxy. The proxy channel below is the advanced, deep-attribution
  // option. (Ordering mirrors the README Quick Start and the product's stated default.)
  const otlpUrl = (() => {
    try {
      const u = new URL(baseUrl);
      return `${u.protocol}//${u.hostname}:4318`;
    } catch {
      return "http://127.0.0.1:4318";
    }
  })();
  const otelRows = otelEnv(otlpUrl).map(([k, v]) =>
    el("tr", {}, [
      el("td", { class: "cause", text: k }),
      el("td", { class: "num", text: v }),
      el("td", {}, [copyBtn(v)]),
    ])
  );
  // Live receiver status (both transports). Shows whether anything is actually being captured.
  let otlpLine = "Receiver status unavailable.";
  // Hook-health: a distinct line so a registered-but-silent lifecycle hook shows
  // as a fixable warning, not silently-missing session state. null = don't render the line.
  let hookLine: string | null = null;
  // The unhealthy case leads with plain-language IMPACT + hides the technical remedy behind a
  // disclosure, instead of a jargon wall ("authoritative", "disableAllHooks").
  let hookNode: HTMLElement | null = null;
  try {
    const s = await client.otlpStatus();
    otlpLine = s.listening
      ? s.events > 0
        ? `OTel channel listening on :${s.port}. ${s.events} event${s.events === 1 ? "" : "s"} captured${s.age_seconds == null ? "" : `, last ${s.age_seconds}s ago`}.`
        : `OTel channel listening on :${s.port}. No events captured yet. Enable telemetry below.`
      : "OTel channel not listening.";
    // Only speak to hooks once capture is otherwise working (else the OTLP line already tells the
    // story). Hooks power authoritative Live state; silent hooks fall back to recency + process.
    if (s.listening && s.events > 0) {
      const seen = s.hooks_seen ?? 0;
      if (seen > 0) {
        hookLine = `Lifecycle hooks firing (${seen} event${seen === 1 ? "" : "s"}). Live session state is authoritative.`;
      } else {
        hookNode = el("div", { class: "caption" }, [
          "⚠ Session start and end times may be approximate. Without lifecycle hooks, Tare guesses when a session begins and ends from recent activity.",
          el("details", { class: "hook-fix" }, [
            el("summary", { text: "Fix this" }),
            el("p", {
              class: "sub",
              text: "Run `tare connect` to register the lifecycle hooks, and make sure `disableAllHooks` isn't set in your agent's config. Then restart the agent.",
            }),
          ]),
        ]);
      }
    }
  } catch {
    otlpLine = "OTel channel not reachable.";
  }
  frag.appendChild(
    el("section", { class: "section" }, [
      el("h2", {}, ["Connect an agent (optional, richer real-time)"]),
      // Capture already works with zero setup through JSONL session files; connecting is an
      // opt-in upgrade for live per-request detail, not a prerequisite.
      el("p", {
        class: "caption",
        text: "You don't need this to see your Claude Code spend. Tare already reads Claude Code JSONL session files from ~/.claude with zero setup. Connecting is a reversible live-detail upgrade, not a replacement for the zero-setup JSONL lane.",
      }),
      el("p", { class: "caption", text: otlpLine }),
      ...(hookLine ? [el("p", { class: "caption", text: hookLine })] : []),
      ...(hookNode ? [hookNode] : []),
      el("p", {
        class: "caption",
        text: "Capture token usage without routing model traffic through Tare. These keys make Claude Code (and Codex or SDK apps) export usage to Tare's local OTel channel. They do not replace your existing ANTHROPIC_BASE_URL or corporate proxy; capture never sits in the request path, and nothing leaves your machine.",
      }),
      el("table", {}, [el("tbody", {}, otelRows)]),
      el("p", { class: "caption", text: "Or paste this block, then restart your agent:" }),
      el("pre", { class: "explain", text: otelEnvShell(otlpUrl) }),
      copyBtn(otelEnvShell(otlpUrl), "Copy all"),
    ])
  );

  // ---- per-agent recipes: the "client of choice" onramp beyond Claude Code + Codex ----
  {
    const agentSel = el(
      "select",
      { "aria-label": "Agent" },
      AGENT_RECIPES.map((a) => el("option", { value: a.id, text: a.label }))
    ) as HTMLSelectElement;
    const note = el("p", { class: "caption" });
    const pre = el("pre", { class: "explain" });
    const copy = el("button", {
      class: "btn ghost",
      text: "Copy",
      onClick: () => void navigator.clipboard?.writeText(pre.textContent ?? ""),
    });
    const renderRecipe = (): void => {
      const r = agentRecipe(agentSel.value);
      note.textContent = `${r.mode === "otel" ? "OTel channel" : "Proxy channel"}: ${r.note}`;
      pre.textContent = r.render(otlpUrl, baseUrl);
    };
    agentSel.addEventListener("change", renderRecipe);
    renderRecipe();
    frag.appendChild(
      el("section", { class: "section" }, [
        el("h2", {}, ["Other agents"]),
        el("p", { class: "caption", text: "Point any supported client at Tare. OTel-capable agents export usage; OpenAI-compatible clients route through the proxy channel. Configured per agent and reversible." }),
        el("label", { class: "sub" }, ["Agent ", agentSel]),
        note,
        pre,
        copy,
      ])
    );
  }

  // ---- capture health: is DATA flowing? ----
  const cov = await client.coverage().catch(() => null);
  if (cov && cov.status !== "none") {
    const tone = cov.status === "green" ? "ok" : cov.status === "amber" ? "warn" : "error";
    const msg =
      cov.status === "green"
        ? "Proxy and OTel are both feeding cost data. Full coverage."
        : cov.status === "amber"
          ? `Capturing via ${cov.has_otel ? "OTel" : "proxy"} only. The other channel isn't feeding cost data yet.`
          : "An agent is active (heartbeats) but no cost data is being captured. Check that telemetry is enabled and pointed at the OTel channel.";
    const block = el("section", { class: "section" }, [
      el("h2", {}, ["Capture health"]),
      el("p", { class: "caption" }, [
        icon("dot", { size: 8, class: `capture-dot capture-${tone}` }),
        el("span", { text: ` ${msg}` }),
      ]),
    ]);
    if (cov.blind_sources.length > 0) {
      block.appendChild(
        el("p", { class: "error", text: `No cost data: ${cov.blind_sources.join(", ")} sent heartbeats, but no cost steps.` })
      );
    }
    frag.appendChild(block);
  }

  // ---- advanced: proxy channel (deep prompt-component attribution) ----
  // This sits in the request path (a base URL swap), so it's opt-in — but it's the only path that
  // attributes spend WITHIN a prompt (system / tool-defs / history / tool-results).
  frag.appendChild(
    el("section", { class: "section" }, [
      el("h2", {}, ["Advanced: deep attribution via the proxy channel"]),
      el("p", {
        class: "caption",
        text: "Routes a run through Tare's local proxy channel via a one-line base URL swap. Use this when you want the full prompt-component flamegraph; otherwise the recommended OTel channel above is enough.",
      }),
    ])
  );

  // Local service (desktop): surface the already-running embedded daemon as a managed,
  // observable service — proxy serve-state + HTTP port, the OTLP receiver's health (same pill Live
  // uses), and today's Claude Code metrics cross-check, with Start/Stop wired to the existing commands.
  if (canControl) {
    // The daemon's two ports report independently; fetch both plus the Claude Code cross-check.
    let otlp: OtlpStatus | null = null;
    let vendor: VendorToday | null = null;
    try {
      otlp = await client.otlpStatus();
    } catch {
      /* receiver unreachable — paintCapturePill renders the error state from null */
    }
    try {
      vendor = await client.vendorToday();
    } catch {
      /* no Claude Code cross-check — the row is simply omitted */
    }

    // Proxy row: a service state pill + the HTTP port + Start/Stop.
    const btn = el("button", {
      class: "btn primary",
      text: status.running ? "Stop proxy channel" : "Start proxy channel",
    });
    // Error sink that lives inside the section (not the spent fragment), so failures are visible.
    const proxyErr = el("div");
    btn.addEventListener("click", async () => {
      (btn as HTMLButtonElement).disabled = true;
      proxyErr.replaceChildren();
      try {
        if (status.running) await client.proxyStop();
        else await client.proxyStart();
        await renderConnect(root, client); // re-render with the new status
      } catch (e) {
        (btn as HTMLButtonElement).disabled = false;
        proxyErr.replaceChildren(el("p", { class: "error", text: String(e) }));
      }
    });
    const proxyRow = el("div", { class: "service-row" }, [
      statePill(status.running ? "running" : "stopped"),
      el("span", {
        class: "service-label",
        text: status.running ? `Proxy channel: running on :${status.port}` : "Proxy channel: stopped",
      }),
      btn,
    ]);

    // OTel-channel row: the exact capture-health pill Live paints, so the two screens agree. Labelled
    // as the second channel of the capture service.
    const capturePill = el("span", { class: "capture-pill" });
    paintCapturePill(capturePill, otlp);
    const otlpRow = el("div", { class: "service-row" }, [
      capturePill,
      el("span", { class: "service-label", text: "OTel channel" }),
    ]);

    // Claude Code metrics cross-check row, when Claude Code emitted it.
    const rows = [proxyRow, otlpRow];
    if (vendor && vendor.available) {
      rows.push(
        el("p", {
          class: "caption",
          title: toDollarString(vendor.cost_micros),
          text: `Claude Code reported today: ${fmtUsd(vendor.cost_micros)} · ${fmtTokens(vendor.tokens)} tokens (cross-check, not added to the estimate)`,
        })
      );
    }
    rows.push(proxyErr);

    frag.appendChild(
      el("section", { class: "section local-service" }, [el("h2", {}, ["Capture service"]), ...rows])
    );
  }

  // connect env (base URL swap) — driven by a provider picker so the user sees ONLY their
  // provider's base-URL row(s), not a wall of every SDK's env vars. The choice is remembered and
  // pre-selects the picker next time (and in onboarding).
  let selected = providerPreset(providerPref()).id;
  const envBody = el("tbody");
  const hint = el("p", { class: "caption" });
  const pricingLine = el("p", { class: "caption pricing-freshness" });
  const pasteBlock = el("pre", { class: "explain" });
  const copyAll = el("span");
  // Pricing freshness/coverage is read once; it changes only with the bundled table.
  const pricingInfo = await client.pricing().catch(() => null);

  const renderProviderEnv = () => {
    const preset = providerPreset(selected);
    hint.textContent = preset.hint;
    // Lead with the ONE recommended capture mode for this provider.
    const reco = captureRecommendation(preset.id);
    recoBanner.replaceChildren(
      el("span", { class: "reco-tag", text: "Recommended" }),
      el("span", {}, [
        el("strong", { text: `${preset.label}: ` }),
        `Use ${reco.label}. ${reco.why} `,
        el("span", { class: "sub", text: "The other mode stays available below under Advanced." }),
      ])
    );
    pricingLine.textContent = pricingInfo
      ? pricingFreshnessLine(pricingInfo.version, pricingInfo.effective_date, preset.id, pricingInfo.models_by_provider)
      : "";
    const pairs = connectEnvFor(preset.id, baseUrl);
    if (pairs.length === 0) {
      // Bedrock and the like: no portable base-URL override. Say so rather than show an empty table.
      envBody.replaceChildren(
        el("tr", {}, [el("td", { class: "cause", text: "No override" }), el("td", { text: "No base-URL override for this provider; use the recommended OTel path above." }), el("td", {})])
      );
      pasteBlock.textContent = "";
      copyAll.replaceChildren();
      return;
    }
    envBody.replaceChildren(
      ...pairs.map(([k, v]) =>
        el("tr", {}, [
          el("td", { class: "cause", text: k }),
          el("td", { class: "num", text: v }),
          el("td", {}, [copyBtn(v)]),
        ])
      )
    );
    const shell = connectEnvShellFor(preset.id, baseUrl);
    pasteBlock.textContent = shell;
    copyAll.replaceChildren(copyBtn(shell, "Copy all"));
  };

  const picker = el(
    "select",
    {
      class: "input provider-picker",
      "aria-label": "Provider",
      onChange: (e: Event) => {
        selected = (e.target as HTMLSelectElement).value as ProviderId;
        setProviderPref(selected);
        renderProviderEnv();
      },
    },
    PROVIDER_PRESETS.map((p) =>
      el("option", { value: p.id, text: p.label, ...(p.id === selected ? { selected: "" } : {}) })
    )
  );
  renderProviderEnv();

  frag.appendChild(
    el("section", { class: "section" }, [
      el("h2", {}, ["Route your agent through the proxy channel"]),
      el("label", {}, [el("span", { class: "field-label", text: "Your provider" }), picker]),
      hint,
      pricingLine,
      el("p", {
        class: "caption",
        text: canControl
          ? "Start the proxy channel, then set these in your agent's environment so its SDK routes through Tare."
          : "Set these in your agent's environment so its SDK routes through Tare. This app's origin already uses the proxy channel.",
      }),
      el("table", {}, [envBody]),
      el("p", { class: "caption", text: "Or paste this block:" }),
      pasteBlock,
      copyAll,
    ])
  );

  root.replaceChildren(frag);
}
