// First-run onboarding as a pillar-shaped, capture-first arc. The one moment
// to teach the product story is spent on the story, not on settings:
//   1. Capture  — confirm zero-setup Claude Code capture, with optional live-detail setup.
//   2. Profile  — wait for the first event and flip to success the moment spend is captured.
//   3. Tune     — a spend cap + privacy, OPTIONAL and last, never the gate.
// Per-step progress is persisted so an un-wired returning user resumes at Capture/Profile rather
// than restarting. Config is written on the desktop, or shown as copyable tare.toml in the browser.

import { el } from "../ui/el.js";
import { humanizeKey } from "../ui/format.js";
import { privacyProfileDescription } from "../ui/privacyProfiles.js";
import { defineTerm } from "../ui/glossary.js";
import {
  setOnboarded,
  onboardStep,
  setOnboardStep,
} from "../ui/prefs.js";
import { otelEnvShell } from "../ui/connectEnv.js";
import { navigate, routePath } from "../ui/store.js";
import type { TareClient, TareConfigDto } from "../client.js";

const PROFILES = ["strict_counts", "fingerprint", "max_private", "max_inspect"];
const OTLP_URL = "http://127.0.0.1:4318"; // receiver default (matches Connect's fallback)

/// Readable text for a thrown value (Error message or its string form).
function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/// If the first captured event hasn't arrived within this long, surface a troubleshooting link
/// — the poll keeps running, but the user is no longer left staring at "Waiting…".
const TROUBLESHOOT_AFTER_MS = 30_000;

export async function renderOnboarding(root: HTMLElement, client: TareClient): Promise<void> {
  const canSave = client.canSaveConfig?.() ?? false;
  // Replaying onboarding is an edit of two fields, not a reset. Load the current values so the
  // form neither snaps back to defaults nor drops advanced fields in those same TOML sections.
  const existingConfig = canSave ? await client.config().catch(() => undefined) : undefined;
  let step = Math.min(2, onboardStep());
  // Chosen but not yet persisted; written only when the user finishes (or skips) the Tune step.
  let capUsd = existingConfig?.budget.max_spend_usd?.toString() ?? "1.00";
  let profileName = existingConfig?.privacy.profile ?? "strict_counts";
  // The Profile step's wait-for-event poll. Hoisted so any step change cancels it — navigating
  // between steps only replaceChildren()s (root stays connected), so a step-local self-cancel on
  // root.isConnected wouldn't fire (poll-lifecycle fix).
  let pollTimer: ReturnType<typeof setInterval> | undefined;
  // Hoisted like pollTimer: render() must cancel it on a step change too, or a stale
  // 30s troubleshoot timer fires later on a detached node.
  let troubleshootTimer: ReturnType<typeof setTimeout> | undefined;

  const go = (s: number): void => {
    step = Math.max(0, Math.min(2, s));
    setOnboardStep(step);
    render();
  };
  // Whether tare has captured ANY spend — today OR historical. A heavy existing user's
  // Claude Code sessions are mostly from prior days, so `today()` alone reads 0 and would wrongly stall
  // onboarding / seed demo data over their real history. `sessions()` is source-agnostic and spans all
  // time, so a session that already caught up from JSONL counts.
  const hasCapturedSpend = async (): Promise<boolean> => {
    try {
      if ((await client.today()).total_micros > 0) return true;
      return (await client.sessions()).rows.length > 0;
    } catch {
      return false;
    }
  };
  const lastRunId = async (): Promise<string | null> => {
    const runs = await client.listRuns();
    return runs[runs.length - 1] ?? null;
  };
  const capturedAtStart = await hasCapturedSpend();
  let latestRunId: string | null = null;
  if (capturedAtStart) {
    try {
      latestRunId = await lastRunId();
    } catch {
      /* Pulse remains a truthful fallback if the run index is temporarily unavailable. */
    }
  }
  const done = async (destination: "pulse" | "run" = "pulse"): Promise<void> => {
    setOnboarded(true);
    // Never leave a first-run user on an empty product. Seed the clearly-labelled sample only when
    // there is no captured history, and retain its id so the Run Profile CTA opens what it promises.
    try {
      if (!(await hasCapturedSpend())) latestRunId = await client.seedDemo();
      else if (!latestRunId) latestRunId = await lastRunId();
    } catch {
      /* seeding/index lookup is best-effort; Pulse still gives a valid destination */
    }
    if (destination === "run" && latestRunId) {
      window.location.hash = routePath(["investigate", "run", latestRunId]);
    } else {
      navigate("pulse");
    }
  };

  // ---- shared chrome: a 3-dot stepper ----
  const stepper = (): HTMLElement =>
    el(
      "div",
      { class: "onboard-steps" },
      // "See spend" (verb-goal), NOT "Profile" — which collided with the Privacy profile control in
      // step 3 (one word, two unrelated meanings).
      ["Capture", "See spend", "Tune"].map((label, i) =>
        el("span", { class: `onboard-step${i === step ? " active" : ""}${i < step ? " done" : ""}`, text: `${i + 1}. ${label}` })
      )
    );

  // ---- Step 1: Capture ----
  function capture(): HTMLElement {
    // Connect is now OPTIONAL: JSONL capture reads ~/.claude with zero setup, so the
    // required path is just "open Tare". Everything below (env snippet, restart, capture-service
    // button) is a richer-real-time UPGRADE tucked into an optional disclosure — not a prerequisite.
    const snippet = otelEnvShell(OTLP_URL);
    const optionalKids: (HTMLElement | string)[] = [
      el("p", { class: "sub", text: "For live per-request detail, use this one additive OTel path for Claude Code or Codex, then restart the agent. Provider-specific and proxy options stay in the Capture sheet." }),
      el("pre", { class: "explain", text: snippet }),
      el("button", {
        class: "btn ghost",
        type: "button",
        text: "Copy live configuration",
        onClick: () => {
          try {
            void navigator.clipboard?.writeText(snippet);
          } catch {
            /* The block remains selectable manually. */
          }
        },
      }),
      el("a", {
        class: "btn ghost",
        href: routePath(["pulse"], { sheet: "capture" }),
        text: "Open full Capture setup →",
      }),
    ];
    if (client.canControlProxy?.()) {
      // Optional: ensure the capture service is running (the desktop app already auto-starts it). Never
      // swallow a failure: in-flight state on click, inline error, button re-armed.
      const startErr = el("p", { class: "error onboard-start-err" });
      startErr.hidden = true;
      const startBtn = el("button", { class: "btn", text: "Start capture service" }) as HTMLButtonElement;
      startBtn.onclick = async () => {
        startBtn.disabled = true;
        startBtn.textContent = "Starting…";
        startErr.hidden = true;
        try {
          await client.proxyStart();
          startBtn.textContent = "Capture service running ✓";
        } catch (e) {
          startBtn.disabled = false;
          startBtn.textContent = "Start capture service";
          startErr.textContent = `Couldn't start the capture service: ${errText(e)}. Make sure nothing else holds the port, then retry.`;
          startErr.hidden = false;
        }
      };
      optionalKids.push(startBtn, startErr);
    }
    const kids: (HTMLElement | string)[] = [
      el("h2", {}, [capturedAtStart ? "1 · Captured history found" : "1 · Ready to capture"]),
      el("p", {
        class: "caption",
        text: capturedAtStart
          ? "Tare found local Claude Code session history and can show its estimated cost now. No account is required, and nothing leaves this machine."
          : "No captured history is available yet. Tare automatically reads supported local Claude Code sessions; use the optional setup below for live agent detail, or continue to load the clearly labelled sample.",
      }),
      el("details", { class: "onboard-optional" }, [
        el("summary", {}, ["Optional: richer real-time detail (connect an agent)"]),
        ...optionalKids,
      ]),
      el("div", { class: "onboard-nav" }, [el("button", { class: "btn primary", text: "Next → See spend", onClick: () => go(1) })]),
    ];
    return el("section", { class: "section onboard" }, [stepper(), ...kids]);
  }

  // ---- Step 2: Profile (wait for the first event, flip to success) ----
  function profile(): HTMLElement {
    const status = el("p", {
      class: "caption",
      text: capturedAtStart
        ? "Checking your captured history…"
        : "No captured spend yet. Explore the clearly labelled sample now, or leave this open while Tare waits for your first event.",
    });
    const spinner = el("span", { class: "onboard-spinner", "aria-hidden": "true" });
    const seeBtn = el("button", {
      class: "btn primary",
      text: capturedAtStart ? "Open Run Profile →" : "Explore sample Run Profile →",
      onClick: () => void done("run"),
    }) as HTMLButtonElement;
    // A first-run user without capture should never reach a disabled dead end: this CTA seeds and
    // opens the bundled, explicitly labelled sample. If real data arrives first, check() retargets
    // the same button to that latest run without creating a competing path.
    seeBtn.disabled = capturedAtStart;
    const statusRow = el("div", { class: "onboard-wait" }, [spinner, status]);
    // If nothing lands within the window, stop leaving the user staring at "Waiting…" — reveal a
    // troubleshooting link (the poll keeps running in case the event arrives later).
    const troubleshoot = el("p", { class: "caption onboard-troubleshoot" }, [
      "No events yet? ",
      el("a", { href: routePath(["pulse"], { sheet: "capture" }), text: "Check the capture setup →" }),
    ]);
    troubleshoot.hidden = true;
    troubleshootTimer = setTimeout(() => {
      troubleshoot.hidden = false;
    }, TROUBLESHOOT_AFTER_MS);

    // Poll until the first event lands, then flip to success. Cancelled by render() on any step
    // change and self-cancels if the whole screen leaves the DOM.
    const check = async (): Promise<void> => {
      if (!root.isConnected) return;
      try {
        // Advance on ANY captured spend, today or historical — a heavy existing user's
        // sessions already caught up from JSONL, so don't make them wait for a NEW turn today.
        const todayMicros = (await client.today()).total_micros;
        const captured = todayMicros > 0 || (await client.sessions()).rows.length > 0;
        if (captured) {
          try {
            latestRunId = (await lastRunId()) ?? latestRunId;
          } catch {
            /* keep the truthful Pulse fallback when the run index is unavailable */
          }
          status.textContent =
            todayMicros > 0
              ? "✓ Spend captured. You're profiling for real."
              : "✓ Found your sessions. Tare is tracking your spend.";
          statusRow.classList.add("captured");
          spinner.remove();
          troubleshoot.hidden = true;
          clearTimeout(troubleshootTimer);
          seeBtn.disabled = false;
          seeBtn.textContent = latestRunId ? "Open Run Profile →" : "Open Pulse →";
          if (pollTimer) clearInterval(pollTimer);
        }
      } catch {
        /* keep waiting */
      }
    };
    pollTimer = setInterval(() => {
      if (!root.isConnected) {
        if (pollTimer) clearInterval(pollTimer);
        clearTimeout(troubleshootTimer);
        return;
      }
      void check();
    }, 2000);
    void check();

    return el("section", { class: "section onboard" }, [
      stepper(),
      el("h2", {}, ["2 · See your first spend"]),
      el("p", { class: "caption" }, [
        "The moment Tare sees a priced call, the app comes alive: one ",
        defineTerm("flamegraph"),
        " of where the money went, sliced every way.",
      ]),
      statusRow,
      troubleshoot,
      el("div", { class: "onboard-nav" }, [
        el("button", { class: "btn ghost", text: "← Back", onClick: () => go(0) }),
        seeBtn,
        el("button", { class: "btn ghost", text: "Skip to tuning →", onClick: () => go(2) }),
      ]),
    ]);
  }

  // ---- Step 3: Tune (optional; not a gate) ----
  function tune(): HTMLElement {
    const maxSpend = el("input", { type: "number", value: capUsd }) as HTMLInputElement;
    const prof = el("select", {}, PROFILES.map((p) => el("option", { value: p, text: humanizeKey(p), ...(p === profileName ? { selected: "" } : {}) }))) as HTMLSelectElement;
    // Inline one-line tradeoff for the selected profile — privacy is high-stakes, so the
    // meaning is legible in place, not tooltip-only.
    const profDesc = el("p", { class: "caption sub", text: privacyProfileDescription(prof.value) });
    prof.addEventListener("change", () => {
      profDesc.textContent = privacyProfileDescription(prof.value);
    });
    const out = el("div");

    const buildCfg = (): { cfg: Partial<TareConfigDto>; cap?: number } => {
      const parsed = Number(maxSpend.value);
      const cap = maxSpend.value.trim() !== "" && Number.isFinite(parsed) && parsed >= 0 ? parsed : undefined;
      return {
        cap,
        // Send ONLY the two sections onboarding actually asks about. It used to also send
        // `providers: {}` and `proxy: {}`, which — because saving is a whole-file rewrite —
        // wiped `[[providers.local_overlay]]` and the `[proxy]` port/db/pricing paths off the
        // disk of anyone who replayed onboarding from Settings. An omitted
        // section is preserved by the backend merge; a present-but-empty one is a deliberate clear.
        cfg: {
          budget: { ...existingConfig?.budget, max_spend_usd: cap },
          privacy: { ...existingConfig?.privacy, profile: prof.value },
        },
      };
    };
    const finish = async (): Promise<void> => {
      capUsd = maxSpend.value;
      profileName = prof.value;
      const { cfg, cap } = buildCfg();
      if (canSave) {
        try {
          await client.saveConfig(cfg);
          await done();
        } catch (e) {
          out.replaceChildren(el("p", { class: "error", text: String(e) }));
        }
      } else {
        // Browser: can't write the file — show the equivalent tare.toml to paste, then finish.
        const toml =
          (cap === undefined ? "[budget]\n" : `[budget]\nmax_spend_usd = ${cap}\n`) + `\n[privacy]\nprofile = "${prof.value}"\n`;
        out.replaceChildren(
          el("p", { class: "caption", text: "Save this as tare.toml next to where you run Tare:" }),
          el("pre", { class: "explain", text: toml }),
          el("button", { class: "btn", text: "Done → Pulse", onClick: () => void done() })
        );
      }
    };

    return el("section", { class: "section onboard" }, [
      stepper(),
      el("h2", {}, ["3 · Tune (optional): cap + privacy"]),
      el("p", { class: "caption", text: "Sensible defaults already apply. This step is optional. Set a per-run spend cap (warns/stops a runaway) and a privacy posture; change either anytime in Settings." }),
      el("label", { class: "field" }, [el("span", { class: "field-label", text: "Per-run spend cap (USD)" }), maxSpend]),
      el("label", { class: "field" }, [el("span", { class: "field-label", text: "Privacy profile" }), prof]),
      profDesc,
      el("div", { class: "onboard-nav" }, [
        el("button", { class: "btn ghost", text: "← Back", onClick: () => go(1) }),
        el("button", { class: "btn primary", text: canSave ? "Finish" : "Show me the config", onClick: () => void finish() }),
        el("button", { class: "btn ghost", text: "Skip, use defaults", onClick: () => void done() }),
      ]),
      out,
    ]);
  }

  function render(): void {
    // Cancel any prior step's poll before swapping — root stays connected across a step change,
    // so the poll's own isConnected self-cancel wouldn't catch this.
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = undefined;
    }
    if (troubleshootTimer) {
      clearTimeout(troubleshootTimer);
      troubleshootTimer = undefined;
    }
    root.replaceChildren([capture, profile, tune][step]());
  }
  render();
}
