import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { renderSettings } from "../src/screens/settings.js";
import { renderOnboarding } from "../src/screens/onboarding.js";
import { fakeClient } from "./fakeClient.js";
import { isOnboarded, setOnboarded, setOnboardStep } from "../src/ui/prefs.js";
import { themePreference } from "../src/ui/theme.js";
import type { TareConfigDto } from "../src/client.js";
import type { SavedInvestigationV2 } from "../src/analysis/investigation.js";

const CFG: TareConfigDto = {
  budget: { max_spend_usd: 0.5, max_steps: 100 },
  privacy: { profile: "strict_counts" },
  providers: {},
  proxy: { port: 8788 },
};

describe("Settings screen", () => {
  it("loads config and saves an edited capture config (desktop transport)", async () => {
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => CFG,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    // Pre-filled from config.
    const spendField = Array.from(root.querySelectorAll(".field")).find(
      (row) => row.querySelector(".field-label")?.textContent === "Max spend (USD)"
    ) as HTMLElement;
    const spend = spendField.querySelector('input[type="number"]') as HTMLInputElement;
    expect(spend.value).toBe("0.5");
    // Edit + save.
    spend.value = "2";
    (Array.from(root.querySelectorAll("button")).find((b) => b.textContent?.includes("Save")) as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved).not.toBeNull();
    expect(saved!.budget?.max_spend_usd).toBe(2);
  });

  // Regression guard for the Settings data-loss bug. Saving is a whole-file rewrite of
  // tare.toml, so any section this sheet does not model must still be echoed back. Building the
  // payload from scratch silently deleted `[[unit]]` and `[[lineage]]` — the config behind
  // `tare unit` / `tare lineage` — from the user's disk on every save.
  it("echoes back config sections it does not edit, so a save never deletes them", async () => {
    const rich: TareConfigDto = {
      ...CFG,
      budget: { ...CFG.budget, soft_spend_usd: 0.25 },
      privacy: { profile: "strict_counts", suppress_latency: true, git_attribution: true },
      unit: [{ name: "pull-request", match: { tag: "pr" } }],
      lineage: [{ name: "system-prompt", versions: [{ label: "v1", hash: 42 }] }],
      alert: [{ metric: "today_spend", threshold: 5 }],
      capture: { mode: "always_on", jsonl: true },
      ui: { tz_offset_minutes: -420 },
    };
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => rich,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    (Array.from(root.querySelectorAll("button")).find((b) => b.textContent?.includes("Save")) as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));

    expect(saved).not.toBeNull();
    // The sections Settings has no UI for must come back untouched.
    expect(saved!.unit).toEqual(rich.unit);
    expect(saved!.lineage).toEqual(rich.lineage);
    // ...and the ones it does model must still round-trip rather than reset.
    expect(saved!.alert).toEqual(rich.alert);
    expect(saved!.ui?.tz_offset_minutes).toBe(-420);
    expect(saved!.budget?.soft_spend_usd).toBe(0.25);
    expect(saved!.privacy?.suppress_latency).toBe(true);
    expect(saved!.privacy?.git_attribution).toBe(true);
  });

  // Onboarding is reachable from Settings' "Replay onboarding" button, so an already-configured
  // user can run it again. It must not send sections it never asked about: a present-but-empty
  // section is a deliberate wholesale clear, which is how `providers: {}` and `proxy: {}` used to
  // wipe the local-overlay rows and the proxy port/db/pricing paths.
  it("onboarding sends only the sections it asks about", async () => {
    setOnboarded(false);
    setOnboardStep(3);
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => CFG,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderOnboarding(root, client);
    // Step 3 ("Tune") is where the config is written; its primary button is "Finish" when the
    // transport can save. Assert unconditionally — a skipped assertion is not a guard.
    const finish = Array.from(root.querySelectorAll("button")).find(
      (b) => b.textContent === "Finish"
    ) as HTMLButtonElement;
    expect(finish, "step 3 must offer Finish when canSaveConfig() is true").toBeTruthy();
    finish.click();
    await new Promise((r) => setTimeout(r, 0));

    expect(saved).not.toBeNull();
    expect(Object.keys(saved!).sort()).toEqual(["budget", "privacy"]);
  });

  it("saves the chosen capture mode", async () => {
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => CFG,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    // The capture-mode select is the one offering the always_on option.
    const sel = Array.from(root.querySelectorAll("select")).find((s) =>
      Array.from(s.options).some((o) => o.value === "always_on")
    ) as HTMLSelectElement;
    expect(sel).toBeTruthy();
    sel.value = "always_on";
    sel.dispatchEvent(new Event("change"));
    (
      Array.from(root.querySelectorAll("button")).find((b) =>
        b.textContent?.includes("Save")
      ) as HTMLButtonElement
    ).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved).not.toBeNull();
    expect(saved!.capture?.mode).toBe("always_on");
  });

  describe("background-on-close toggle", () => {
    const findToggle = (root: HTMLElement): HTMLInputElement | null => {
      const lbl = Array.from(root.querySelectorAll(".field-label")).find(
        (s) => s.textContent === "Close keeps Tare running in the background"
      );
      return (lbl?.parentElement?.querySelector('input[type="checkbox"]') as HTMLInputElement) ?? null;
    };
    const setOs = (os: string) => {
      document.documentElement.dataset.os = os;
    };
    beforeEach(() => setOs("windows"));
    afterEach(() => delete document.documentElement.dataset.os);

    it("shows on Windows/Linux desktop, reflects the persisted value, and persists on toggle", async () => {
      let persisted: boolean | null = null;
      const client = fakeClient({
        config: async () => CFG,
        canBackgroundOnClose: () => true,
        getBackgroundOnClose: async () => true,
        setBackgroundOnClose: async (v) => {
          persisted = v;
        },
      });
      const root = document.createElement("div");
      await renderSettings(root, client);
      await new Promise((r) => setTimeout(r, 0)); // let the async getBackgroundOnClose resolve
      const toggle = findToggle(root);
      expect(toggle).toBeTruthy();
      expect(toggle!.checked).toBe(true); // reflects the persisted preference
      toggle!.checked = false;
      toggle!.dispatchEvent(new Event("change"));
      expect(persisted).toBe(false);
    });

    it("is hidden on macOS, where the app always keeps running", async () => {
      setOs("macos");
      const client = fakeClient({ config: async () => CFG, canBackgroundOnClose: () => true });
      const root = document.createElement("div");
      await renderSettings(root, client);
      expect(findToggle(root)).toBeNull();
    });

    it("is hidden on the browser transport (no window-close-to-tray)", async () => {
      const client = fakeClient({ config: async () => CFG, canBackgroundOnClose: () => false });
      const root = document.createElement("div");
      await renderSettings(root, client);
      expect(findToggle(root)).toBeNull();
    });
  });

  it("edits reprice mode and preserves pricing overrides on save", async () => {
    const cfg: TareConfigDto = {
      budget: {},
      privacy: {},
      providers: {},
      proxy: { port: 8788 },
      pricing: {
        reprice: "as-of",
        overrides: [{ model: "my-local", input_usd_per_mtok: 1, output_usd_per_mtok: 2 }],
      },
    };
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => cfg,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    // The reprice select is pre-filled from config (as-of) and switchable to latest.
    const sel = root.querySelector('select[aria-label="Reprice mode"]') as HTMLSelectElement;
    expect(sel).toBeTruthy();
    expect(sel.value).toBe("as-of");
    sel.value = "latest";
    (Array.from(root.querySelectorAll("button")).find((b) => b.textContent?.includes("Save")) as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved).not.toBeNull();
    expect(saved!.pricing?.reprice).toBe("latest");
    // The wipe-fix: overrides survive a Settings save untouched.
    expect(saved!.pricing?.overrides).toEqual([
      { model: "my-local", input_usd_per_mtok: 1, output_usd_per_mtok: 2 },
    ]);
  });

  it("humanizes privacy-profile options and shows a live one-line tradeoff", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    // The profile select shows humanized labels (values stay the raw enum for config).
    const sel = Array.from(root.querySelectorAll("select")).find((s) =>
      Array.from(s.options).some((o) => o.value === "max_inspect")
    ) as HTMLSelectElement;
    expect(sel).toBeTruthy();
    const byValue = (v: string) => Array.from(sel.options).find((o) => o.value === v)!;
    expect(byValue("strict_counts").textContent).toBe("Strict counts");
    expect(byValue("max_inspect").textContent).toBe("Max inspect");
    // A point-of-use description is visible (not tooltip-only) and updates on change.
    const desc = root.querySelector(".privacy-profile-desc")!;
    expect(desc.textContent).toContain("No prompt or response text"); // strict_counts default line
    sel.value = "max_private";
    sel.dispatchEvent(new Event("change"));
    expect(desc.textContent).toContain("No content hashes"); // max_private line
  });

  it("states plainly that capture reads counts only, never prompt text", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    const note = root.querySelector(".jsonl-privacy-note");
    expect(note).toBeTruthy();
    expect(note!.textContent).toContain("~/.claude");
    expect(note!.textContent).toContain("never prompt or response text");
    expect(note!.textContent).toContain("nothing leaves this machine");
  });

  it("offers an independent JSONL capture toggle, on by default", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    const rows = [...root.querySelectorAll("label.field, .field")];
    const jsonlRow = rows.find((r) => /Claude Code JSONL session files/i.test(r.textContent ?? ""));
    expect(jsonlRow).toBeTruthy();
    const box = jsonlRow!.querySelector("input[type=checkbox]") as HTMLInputElement;
    expect(box).toBeTruthy();
    expect(box.checked).toBe(true); // default on (CFG has no capture.jsonl → defaults true)
  });

  it("disables capture-config save on the browser transport but shows the read-only note", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG, canSaveConfig: () => false }));
    const save = Array.from(root.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Save")
    ) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(root.textContent).toContain("Read-only in the browser");
  });

  it("offers max_inspect and purges captured bodies on a two-step confirmation", async () => {
    let purged = 0;
    const root = document.createElement("div");
    await renderSettings(
      root,
      fakeClient({ config: async () => CFG, purgeTranscripts: async () => { purged += 1; } }),
    );
    // max_inspect is a selectable privacy profile with a human-readable label.
    const optValues = Array.from(root.querySelectorAll("option")).map((o) => (o as HTMLOptionElement).value);
    expect(optValues).toContain("max_inspect");
    // The purge button is a two-step confirm: first click arms (no call), second click purges.
    const purgeBtn = Array.from(root.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("Purge captured bodies"),
    ) as HTMLButtonElement;
    expect(purgeBtn).toBeTruthy();
    purgeBtn.click();
    expect(purged).toBe(0); // armed only
    expect(purgeBtn.textContent).toContain("confirm");
    purgeBtn.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(purged).toBe(1);
    expect(root.textContent).toContain("purged");
  });

  it("uses seven Calibrated Bench sections with explicit setting origins", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    const headings = Array.from(root.querySelectorAll(":scope > section.settings-section > h2")).map(
      (heading) => heading.textContent
    );
    expect(headings).toEqual([
      "Appearance",
      "Behavior",
      "Privacy & Capture",
      "Alerts",
      "Data",
      "Shortcuts",
      "Saved investigations",
    ]);
    for (const setting of root.querySelectorAll(".field")) {
      expect(
        setting.querySelector(".field-origin"),
        `missing origin for ${setting.querySelector(".field-label")?.textContent}`
      ).toBeTruthy();
    }
  });

  it("provides sticky jump navigation across all seven sections", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }), "data");
    const nav = root.querySelector('nav[aria-label="Settings sections"]')!;
    const buttons = Array.from(nav.querySelectorAll<HTMLButtonElement>("button"));
    expect(buttons.map((button) => button.textContent)).toEqual([
      "Appearance",
      "Behavior",
      "Privacy & Capture",
      "Alerts",
      "Data",
      "Shortcuts",
      "Investigations",
    ]);
    expect(nav.querySelector('[aria-current="location"]')?.textContent).toBe("Data");
    for (const button of buttons) {
      expect(document.getElementById(button.getAttribute("aria-controls") ?? "") ?? root.querySelector(`#${button.getAttribute("aria-controls")}`)).toBeTruthy();
    }
  });

  it("does not expose the removed Dashboard Layout controls", async () => {
    // Settings never renders controls for the retired card-layout store.
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    expect(root.textContent).not.toMatch(/Dashboard Layout|Live layout/i);
    expect(root.querySelector(".card-toggle")).toBeNull();
  });

  it("lists and deletes durable saved investigations from the local database", async () => {
    const investigation: SavedInvestigationV2 = {
      id: "weekly-cost-review",
      label: "Weekly cost review",
      version: 2,
      state: {
        workspace: "investigate",
        scope: {
          from: null,
          to: null,
          timezone: "UTC",
          entity: "run",
          filters: [],
          pricing: { mode: "effective_dated" },
          metric: "spend_micros",
          normalization: "absolute",
          outcome_denominator: null,
        },
        selection: null,
        baseline: null,
        match: { kind: "aggregate_only" },
        comparison: [],
        pinned: null,
      },
      created_at: "2026-07-01T00:00:00Z",
      updated_at: "2026-07-15T00:00:00Z",
    };
    let rows = [investigation];
    let deleted = "";
    const root = document.createElement("div");
    await renderSettings(
      root,
      fakeClient({
        config: async () => CFG,
        listInvestigations: async () => rows,
        deleteInvestigation: async (id) => {
          deleted = id;
          rows = rows.filter((row) => row.id !== id);
        },
      })
    );
    const section = Array.from(root.querySelectorAll("section")).find(
      (node) => node.querySelector("h2")?.textContent === "Saved investigations"
    ) as HTMLElement;
    expect(section.textContent).toContain("Weekly cost review");
    (section.querySelector('button[aria-label="Delete saved investigation Weekly cost review"]') as HTMLButtonElement).click();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(deleted).toBe("weekly-cost-review");
    expect(section.textContent).not.toContain("Weekly cost review");
  });

  it("exposes System, Light, and Dark appearance in Settings and persists the choice", async () => {
    localStorage.removeItem("tare-theme");
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    // The Appearance control is a select with EXACTLY System / Light / Dark.
    const appearance = Array.from(root.querySelectorAll("select")).find((s) => {
      const vals = Array.from(s.options).map((o) => o.value);
      return vals.length === 3 && vals.includes("system") && vals.includes("light") && vals.includes("dark");
    })!;
    expect(appearance, "Settings exposes an Appearance tri-state").toBeTruthy();
    expect(Array.from(appearance.options).map((o) => o.textContent)).toEqual(["System", "Light", "Dark"]);
    expect(appearance.value).toBe("system"); // fresh install
    // Choosing an explicit override persists it and applies immediately.
    appearance.value = "light";
    appearance.dispatchEvent(new Event("change"));
    expect(themePreference()).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    localStorage.removeItem("tare-theme");
  });
});

describe("capture-first onboarding", () => {
  beforeEach(() => {
    setOnboarded(false);
    setOnboardStep(0); // don't let a prior test's step resume leak in
  });

  it("opens on Capture with one optional live-detail snippet and no provider wall", async () => {
    const root = document.createElement("div");
    await renderOnboarding(root, fakeClient());
    expect(root.querySelector(".onboard-steps")).toBeTruthy();
    // The view leads with zero-setup capture; one OTel snippet is optional and provider-
    // specific alternatives live in the Capture sheet, not in first-run onboarding.
    expect(root.textContent).toContain("1 · Ready to capture");
    expect(root.querySelector("details.onboard-optional")).toBeTruthy();
    expect(root.querySelector("pre")).toBeTruthy(); // a copy-pasteable env snippet (optional)
    expect(root.querySelector("select")).toBeNull();
    expect(root.querySelector("details a")?.getAttribute("href")).toContain("sheet=capture");
    // Next advances to the Profile (waiting) step.
    (root.querySelector(".btn.primary") as HTMLButtonElement).click();
    expect(root.textContent).toContain("2 · See your first spend"); // renamed off "Profile"
    expect(root.querySelector(".onboard-wait")).toBeTruthy();
  });

  it("flips the Profile step to success once spend is captured", async () => {
    setOnboardStep(1);
    const root = document.createElement("div");
    document.body.appendChild(root); // isConnected → the poll runs
    await renderOnboarding(root, fakeClient({ today: async () => ({ total_micros: 4_210_000, pricing_version: "x", effective_date: "2026-06-01" }) }));
    await new Promise((r) => setTimeout(r, 0));
    expect(root.querySelector(".onboard-wait.captured")).toBeTruthy();
    // Spend exists but this fake has no indexed run, so onboarding truthfully offers Pulse instead
    // of promising a Run Profile it cannot open.
    const see = Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "Open Pulse →") as HTMLButtonElement;
    expect(see).toBeTruthy();
    expect(see.disabled).toBe(false);
    root.remove();
  });

  it("cancels the Profile poll when navigating away", async () => {
    vi.useFakeTimers();
    try {
      setOnboardStep(1);
      let calls = 0;
      const root = document.createElement("div");
      document.body.appendChild(root);
      await renderOnboarding(
        root,
        fakeClient({
          today: async () => {
            calls++;
            return { total_micros: 0, pricing_version: "x", effective_date: "2026-06-01" };
          },
        })
      );
      await vi.advanceTimersByTimeAsync(2100); // one poll tick
      const before = calls;
      expect(before).toBeGreaterThan(0);
      // Navigate back to Capture — the poll must stop.
      (Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "← Back") as HTMLButtonElement).click();
      await vi.advanceTimersByTimeAsync(6000); // several more ticks would fire if it leaked
      expect(calls).toBe(before);
      root.remove();
    } finally {
      vi.useRealTimers();
    }
  });

  it("persists config + marks onboarded on Finish (desktop), config LAST", async () => {
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    setOnboardStep(2); // Tune step
    const root = document.createElement("div");
    await renderOnboarding(root, client);
    expect(root.textContent).toContain("3 · Tune (optional)");
    (root.querySelector(".btn.primary") as HTMLButtonElement).click(); // "Finish"
    await new Promise((r) => setTimeout(r, 0));
    expect(saved!.privacy?.profile).toBeTruthy();
    expect(isOnboarded()).toBe(true);
  });

  it("seeds the sample run on Finish when nothing was captured", async () => {
    let seeded = 0;
    setOnboardStep(2);
    const root = document.createElement("div");
    await renderOnboarding(
      root,
      fakeClient({
        canSaveConfig: () => true,
        saveConfig: async () => {},
        today: async () => ({ total_micros: 0, pricing_version: "x", effective_date: "2026-06-01" }),
        seedDemo: async () => {
          seeded++;
          return "demo";
        },
      })
    );
    (root.querySelector(".btn.primary") as HTMLButtonElement).click(); // Finish
    await new Promise((r) => setTimeout(r, 0));
    expect(seeded).toBe(1);
    expect(location.hash).toBe("#/pulse");
  });

  it("does not seed the sample when real spend exists", async () => {
    let seeded = 0;
    setOnboardStep(2);
    const root = document.createElement("div");
    await renderOnboarding(
      root,
      fakeClient({
        canSaveConfig: () => true,
        saveConfig: async () => {},
        today: async () => ({ total_micros: 5_000_000, pricing_version: "x", effective_date: "2026-06-01" }),
        seedDemo: async () => {
          seeded++;
          return "demo";
        },
      })
    );
    (root.querySelector(".btn.primary") as HTMLButtonElement).click(); // Finish
    await new Promise((r) => setTimeout(r, 0));
    expect(seeded).toBe(0);
    expect(location.hash).toBe("#/pulse");
  });

  it("blank cap saves an empty budget (bare table)", async () => {
    let saved: Partial<TareConfigDto> | null = null;
    setOnboardStep(2);
    const root = document.createElement("div");
    await renderOnboarding(root, fakeClient({ canSaveConfig: () => true, saveConfig: async (c) => { saved = c; } }));
    (root.querySelector("input[type=number]") as HTMLInputElement).value = "";
    (root.querySelector(".btn.primary") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved!.budget).toEqual({});
  });

  it("shows a copyable tare.toml on the browser transport", async () => {
    setOnboardStep(2);
    const root = document.createElement("div");
    await renderOnboarding(root, fakeClient({ canSaveConfig: () => false }));
    (root.querySelector(".btn.primary") as HTMLButtonElement).click(); // "Show me the config"
    await new Promise((r) => setTimeout(r, 0));
    expect(root.querySelector("pre")?.textContent).toContain("[budget]");
  });

  it("toggles the change-vs-baseline Runs preference (default on)", async () => {
    localStorage.removeItem("tare-show-baseline-delta");
    const root = document.createElement("div");
    await renderSettings(root, fakeClient());
    const field = Array.from(root.querySelectorAll(".field")).find((f) =>
      f.textContent?.includes("change vs baseline")
    );
    const cb = field?.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(cb).toBeTruthy();
    expect(cb.checked).toBe(true); // default on
    cb.checked = false;
    cb.dispatchEvent(new Event("change"));
    expect(localStorage.getItem("tare-show-baseline-delta")).toBe("0");
    localStorage.removeItem("tare-show-baseline-delta");
  });

  it("adds and saves a declarative alert rule", async () => {
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => CFG,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    const section = Array.from(root.querySelectorAll("section")).find((s) =>
      s.textContent?.includes("Alert rules")
    ) as HTMLElement;
    expect(section).toBeTruthy();
    // Add a today_spend ≥ 5 rule via the metric select + value input + Add.
    const metric = section.querySelector("select") as HTMLSelectElement;
    metric.value = "today_spend";
    const value = section.querySelector('input[type="text"]') as HTMLInputElement;
    value.value = "5";
    const addBtn = Array.from(section.querySelectorAll("button")).find((b) => b.textContent === "Add rule")!;
    addBtn.click();
    expect(section.textContent).toContain("Today spend ≥ 5"); // humanized label
    // Save the config → the rule is persisted.
    const save = Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "Save tare.toml settings")!;
    save.click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved!.alert).toEqual([{ metric: "today_spend", threshold: 5 }]);
  });

  it("adds and saves a local-model cost overlay", async () => {
    let saved: Partial<TareConfigDto> | null = null;
    const client = fakeClient({
      config: async () => CFG,
      canSaveConfig: () => true,
      saveConfig: async (c) => {
        saved = c;
      },
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    const section = Array.from(root.querySelectorAll(".settings-subsection")).find((s) =>
      s.querySelector("h3")?.textContent === "Local model cost overlay"
    ) as HTMLElement;
    expect(section).toBeTruthy();
    // Backend select defaults to ollama; enter a $/1M tokens rate and Add.
    const rate = section.querySelector('input[type="text"]') as HTMLInputElement;
    rate.value = "0.5";
    const addBtn = Array.from(section.querySelectorAll("button")).find((b) => b.textContent === "Add overlay")!;
    addBtn.click();
    expect(section.textContent).toContain("ollama: $0.5/1M tokens (your estimate)");
    // Save → the overlay is persisted under providers.local_overlay.
    (Array.from(root.querySelectorAll("button")).find((b) => b.textContent === "Save tare.toml settings") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved!.providers?.local_overlay).toEqual([{ backend: "ollama", usd_per_mtok: 0.5 }]);
  });

  it("labels field provenance and disables env-overridden fields", async () => {
    const client = fakeClient({
      config: async () => CFG,
      configOrigins: async () => ({
        "privacy.profile": "env",
        "budget.max_spend_usd": "tare.toml",
        "proxy.db": "default",
      }),
    });
    const root = document.createElement("div");
    await renderSettings(root, client);
    const origins = Array.from(root.querySelectorAll(".field-origin")).map((e) => e.textContent);
    expect(origins).toContain("From env (read-only here)");
    expect(origins).toContain("From tare.toml");
    // The env-overridden field (privacy profile select) is disabled so the form can't lie.
    const profileField = Array.from(root.querySelectorAll(".field")).find((f) =>
      f.textContent?.includes("Privacy profile")
    ) as HTMLElement;
    expect((profileField.querySelector("select") as HTMLSelectElement).disabled).toBe(true);
  });

  it("shows a raw-config view with a non-defaults filter", async () => {
    const root = document.createElement("div");
    await renderSettings(root, fakeClient({ config: async () => CFG }));
    const pre = root.querySelector(".raw-config") as HTMLElement;
    expect(pre).toBeTruthy();
    // Full view includes the empty `providers` object.
    expect(pre.textContent).toContain("\"providers\"");
    expect(pre.textContent).toContain("max_spend_usd");
    // Toggling "non-defaults" prunes empty objects (providers {}) but keeps set fields.
    const nd = root.querySelector('.raw-config-wrap input[type="checkbox"]') as HTMLInputElement;
    nd.checked = true;
    nd.dispatchEvent(new Event("change"));
    expect(pre.textContent).not.toContain("\"providers\"");
    expect(pre.textContent).toContain("max_spend_usd");
  });

  it("defaults the landing screen to Pulse and persists a canonical workspace change", async () => {
    localStorage.removeItem("tare-default-screen");
    const root = document.createElement("div");
    await renderSettings(root, fakeClient());
    const sel = Array.from(root.querySelectorAll("select")).find((s) =>
      Array.from(s.options).some((o) => o.value === "optimize")
    ) as HTMLSelectElement;
    expect(sel).toBeTruthy();
    expect(sel.value).toBe("pulse");
    sel.value = "investigate";
    sel.dispatchEvent(new Event("change"));
    expect(localStorage.getItem("tare-default-screen")).toBe("investigate");
    localStorage.removeItem("tare-default-screen");
  });

  it("Finish lands on the dashboard (capture-first arc ends at value, not settings)", async () => {
    location.hash = "#/onboarding";
    setOnboardStep(2);
    const root = document.createElement("div");
    await renderOnboarding(root, fakeClient({ canSaveConfig: () => true, saveConfig: async () => {} }));
    (root.querySelector(".btn.primary") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(location.hash).toBe("#/pulse");
  });

  it("treats a blank cap as bare [budget] and 0 as a hard stop (Tune step)", async () => {
    // Browser blank -> TOML has a bare [budget] (no max_spend_usd line).
    setOnboardStep(2);
    let root = document.createElement("div");
    await renderOnboarding(root, fakeClient({ canSaveConfig: () => false }));
    (root.querySelector("input[type=number]") as HTMLInputElement).value = "";
    (root.querySelector(".btn.primary") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    const toml = root.querySelector("pre")?.textContent ?? "";
    expect(toml).toContain("[budget]");
    expect(toml).not.toContain("max_spend_usd");

    // "0" -> an explicit hard-stop cap of 0.
    let saved: Partial<TareConfigDto> | null = null;
    setOnboardStep(2);
    root = document.createElement("div");
    await renderOnboarding(root, fakeClient({ canSaveConfig: () => true, saveConfig: async (c) => { saved = c; } }));
    (root.querySelector("input[type=number]") as HTMLInputElement).value = "0";
    (root.querySelector(".btn.primary") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(saved!.budget?.max_spend_usd).toBe(0);
  });
});
