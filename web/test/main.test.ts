import { describe, it, expect, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { initialAnalysisStateFromPrefs, mountApp } from "../src/main.js";
import { setOnboarded } from "../src/ui/prefs.js";
import type { Anomaly, TareClient } from "../src/client.js";
import type { FlamegraphModel } from "../src/svg.js";
import type { Report } from "../src/client.js";
import type { TrendReport } from "../src/trendSvg.js";
import { TestTareClient } from "./fakeClient.js";

function golden(name: string): string {
  return readFileSync(resolve(process.cwd(), "../tare-core/tests/golden", name), "utf8");
}

class FakeClient extends TestTareClient implements TareClient {
  async listRuns(): Promise<string[]> {
    return ["bloated_system_prompt"];
  }
  async flamegraph(): Promise<FlamegraphModel> {
    return JSON.parse(golden("flamegraph_bloated.json"));
  }
  async report(): Promise<Report> {
    return JSON.parse(golden("report.json"));
  }
  async trend(): Promise<TrendReport> {
    return JSON.parse(golden("trend.json"));
  }
  async anomalies(): Promise<Anomaly[]> {
    return [];
  }
  async rollup(by: string) {
    return { dimension: by, rows: [], total_micros: 0, pricing_version: "x", estimated: true };
  }
  async vendorToday() {
    return { cost_micros: 0, tokens: 0, day: "2026-06-29", available: false };
  }
  async reconcile() {
    return { day: "2026-06-29", pricing_version: "x", rows: [], estimate_total_micros: 0, vendor_total_micros: 0, delta_total_micros: 0, has_vendor: false };
  }
  async coverage(): ReturnType<TareClient["coverage"]> {
    return { status: "none" as const, has_proxy: false, has_otel: false, blind_sources: [], sources: [] };
  }
  async effectiveness() {
    return { cost_micros: 0, per_pull_request_micros: null, per_commit_micros: null, per_1k_loc_micros: null, per_active_hour_micros: null, per_session_micros: null, accept_rate_pct: null, cost_per_successful_run_micros: null, run_success_rate_pct: null, estimated: true };
  }
  async confidence(): ReturnType<TareClient["confidence"]> {
    return { pricing_age_days: 0, unpriced_token_share_pct: 0, coverage_status: "full", coverage_share_pct: 100, label: "high", estimated: true };
  }
  async today() {
    return { total_micros: 4_210_000, pricing_version: "x", effective_date: "2026-06-01" };
  }
  async runStatus(run_id: string) {
    return { run_id, micros: 1_000_000, steps: 3, top_cause: "bloated-system-prompt" };
  }
  async runStatuses() {
    const ids = await this.listRuns();
    return Promise.all(ids.map((id) => this.runStatus(id)));
  }
  async explain(): Promise<string> {
    return "This run spent mostly on a bloated system prompt.";
  }
  async lenses(): ReturnType<TareClient["lenses"]> {
    return {
      input_micros: 3_000_000,
      output_micros: 1_210_000,
      cache_saved_micros: 0,
      total_micros: 4_210_000,
      calls: 3,
      micros_per_call: 1_403_333,
      total_tokens: 4_000,
      output_tokens: 1_000,
      input_tokens: 3_000,
      cache_read_tokens: 0,
      pricing_version: "x",
      estimated: true,
    };
  }
  // Pulse workspace data: Pulse is now the default landing, so the shell fake must
  // supply its canonical batch (burnrate/budget/failures/loops/savings) + the Now-feed sources.
  async burnrate(): ReturnType<TareClient["burnrate"]> {
    return {
      run_rate_micros_per_day: 140_000,
      effective_rate_micros_per_day: 140_000,
      spent_micros: 1_400_000,
      active_days: 10,
      daily_spend_micros: Array(10).fill(140_000),
      days_elapsed: 10,
      days_in_period: 30,
      projected_micros: 4_200_000,
      projected_low_micros: 3_600_000,
      projected_high_micros: 4_800_000,
      cap_micros: 80_000_000,
      on_track: true,
      headroom_days: 12,
      period: "month",
      period_start: "2026-07-01",
      as_of: "2026-07-10",
      period_end: "2026-07-30",
    };
  }
  async budget() {
    return { period: "month", spent_micros: 4_210_000, cap_micros: 80_000_000, warn_pct: 80, pct: 5, status: "ok" };
  }
  async failures() {
    return { rows: [], total_micros: 0, total_failed_steps: 0, pct_of_spend: 0, pricing_version: "x", estimated: true };
  }
  async loops() {
    return { rows: [], total_micros: 0, total_redundant_steps: 0, pricing_version: "x", estimated: true };
  }
  async savings() {
    return { opportunities: [], total_recoverable_micros: 0, total_spend_micros: 4_210_000, savings_index: 100, pricing_version: "x", estimated: true };
  }
  async sessionsLive() {
    return [];
  }
  async recentSteps() {
    return [];
  }
}

describe("app shell", () => {
  beforeEach(() => {
    setOnboarded(true); // skip the first-run redirect in shell tests
    localStorage.removeItem("tare-pinned-runs");
    localStorage.removeItem("tare-recent-runs");
  });

  it("renders pinned + recent runs in the sidebar", async () => {
    localStorage.setItem("tare-pinned-runs", JSON.stringify(["pinned_run"]));
    localStorage.setItem("tare-recent-runs", JSON.stringify(["pinned_run", "recent_run"]));
    location.hash = "";
    const root = document.createElement("div");
    const c = new FakeClient();
    c.listRuns = async () => ["pinned_run", "recent_run"];
    await mountApp(root, c);
    const runs = root.querySelector(".nav-runs") as HTMLElement;
    const ids = Array.from(runs.querySelectorAll(".nav-run-id")).map((n) => n.textContent);
    // Pinned shown once (deduped out of recents); recent_run also listed.
    expect(ids.filter((i) => i === "pinned_run").length).toBe(1);
    expect(ids).toContain("recent_run");
    expect(runs.querySelector<HTMLAnchorElement>('[title="recent_run"]')?.getAttribute("href")).toBe(
      "#/investigate/run/recent_run"
    );
    const sections = Array.from(runs.querySelectorAll(".nav-section")).map((n) => n.textContent);
    expect(sections).toEqual(["Pinned runs", "Recent runs"]);
    expect(runs.querySelector(".nav-see-all")?.getAttribute("href")).toBe("#/investigate?entity=runs");
    // The complete transport supplies provenance progressively, so recent items show useful
    // context instead of the no-metadata fallback.
    expect(runs.querySelector('[title="recent_run"] .nav-run-meta')?.textContent).toBe("2026-06-29");
    localStorage.removeItem("tare-pinned-runs");
    localStorage.removeItem("tare-recent-runs");
  });

  it("prunes stale navigation destinations without erasing analytical Baseline B", async () => {
    localStorage.setItem("tare-pinned-runs", JSON.stringify(["missing_run", "bloated_system_prompt"]));
    localStorage.setItem("tare-recent-runs", JSON.stringify(["missing_run", "bloated_system_prompt"]));
    localStorage.setItem("tare-baseline-run", "missing_run");
    location.hash = "#/pulse";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());

    expect(root.querySelector('[title="missing_run"]')).toBeNull();
    expect(JSON.parse(localStorage.getItem("tare-pinned-runs") ?? "[]")).toEqual(["bloated_system_prompt"]);
    expect(JSON.parse(localStorage.getItem("tare-recent-runs") ?? "[]")).toEqual(["bloated_system_prompt"]);
    expect(localStorage.getItem("tare-baseline-run")).toBe("missing_run");
  });

  it("renders a saved investigation in the sidebar and removes it on ✕", async () => {
    // SQLite is the sole source; the retired saved-views localStorage bridge is not consulted.
    // The sidebar reads client.listInvestigations() and deletes
    // via client.deleteInvestigation().
    let rows = [
      {
        id: "inv-1",
        label: "Model spend",
        version: 2 as const,
        state: {
          workspace: "investigate",
          scope: {
            from: "2026-07-01",
            to: "2026-07-15",
            filters: [{ op: "eq", dimension: "model", value: "claude-sonnet-4" }],
          },
        },
        created_at: "2026-07-15T00:00:00Z",
        updated_at: "2026-07-15T00:00:00Z",
      },
    ];
    const client = new FakeClient() as FakeClient &
      Pick<TareClient, "listInvestigations" | "deleteInvestigation">;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    client.listInvestigations = async () => rows as any;
    client.deleteInvestigation = async (id: string) => {
      rows = rows.filter((row) => row.id !== id);
    };
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, client);
    const view = root.querySelector(".nav-views .nav-view") as HTMLAnchorElement;
    expect(view.querySelector(".nav-view-title")?.textContent).toBe("Model spend");
    expect(view.querySelector(".nav-view-meta")?.textContent).toBe("Investigate · 2026-07-01–2026-07-15 · 1 filter");
    expect(view?.getAttribute("href")).toBe("#/investigate?investigation=inv-1");
    // ✕ deletes it through the v2 client and removes it from the sidebar (deletion is async).
    (root.querySelector(".nav-view-del") as HTMLButtonElement).click();
    await new Promise((r) => setTimeout(r, 0));
    expect(root.querySelector(".nav-views .nav-view")).toBeNull();
    expect(rows).toEqual([]);
  });

  it("seeds active AnalysisState from legacy baseline/pins without changing transient focus", () => {
    localStorage.setItem("tare-baseline-run", "baseline-r");
    localStorage.setItem("tare-pinned-runs", JSON.stringify(["pinned-r"]));
    const state = initialAnalysisStateFromPrefs();
    expect(state.baseline?.cohort.filters).toEqual([{ op: "run_ids", ids: ["baseline-r"] }]);
    expect(state.pinned).toEqual({ kind: "run", id: "pinned-r", label: "Run pinned-r" });
    expect(state.focus).toEqual({ pane: "canvas", highlighted: null });
    localStorage.removeItem("tare-baseline-run");
    localStorage.removeItem("tare-pinned-runs");
  });

  it("hides the global range control on screens that don't honor it", async () => {
    // The canonical Investigate root resolves every result through the selected range.
    location.hash = "#/investigate";
    const a = document.createElement("div");
    await mountApp(a, new FakeClient());
    expect((a.querySelector(".topbar .range-control") as HTMLElement).hidden).toBe(false);
    // Pulse is the current budget-period forecast and owns no arbitrary analysis range.
    location.hash = "#/pulse";
    const b = document.createElement("div");
    await mountApp(b, new FakeClient());
    expect((b.querySelector(".topbar .range-control") as HTMLElement).hidden).toBe(true);
  });

  it("mounts the sidebar nav, topbar spend pill, and the Pulse workspace as the default landing", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());

    // Shell scaffold: a grouped sidebar (section headers) + a footer for utilities.
    expect(root.querySelector(".shell")).toBeTruthy();
    expect(root.querySelectorAll(".sidebar .nav-section").length).toBeGreaterThanOrEqual(3);
    expect(root.querySelectorAll(".sidebar .nav-item").length).toBeGreaterThanOrEqual(5);
    expect(root.querySelector(".sidebar .nav-footer .nav-item")?.textContent).toBe("Capture");
    // Monitor cut over: Live + Overview are now the single Pulse destination, and it's the default.
    expect(root.querySelector(".sidebar .nav-item.active")?.textContent).toBe("Pulse");
    const navLabels = Array.from(root.querySelectorAll(".sidebar .nav-item")).map((n) => n.textContent);
    expect(navLabels).not.toContain("Live"); // the two old Monitor entries are gone
    expect(navLabels).not.toContain("Overview");
    // No open-tab strip anymore.
    expect(root.querySelector(".tabstrip")).toBeNull();

    // Sidebar sections are the workflow zones.
    const sectionTitles = Array.from(root.querySelectorAll(".sidebar .nav-section")).map((n) => n.textContent);
    expect(sectionTitles.slice(0, 4)).toEqual(["Monitor", "Investigate", "Act", "Views"]); // dynamic run sections may follow
    // Topbar carries the global analysis-range control (Pulse is range-scoped).
    const rangeSel = root.querySelector(".topbar .range-select") as HTMLSelectElement;
    expect(rangeSel).toBeTruthy();
    expect(Array.from(rangeSel.options).map((o) => o.value)).toEqual(["7d", "30d", "90d", "custom"]);

    // Pulse content: one flowing workspace with one hero spend answer, without the old Overview
    // Summary/Segments tabs or card wall.
    expect(root.querySelector(".main .pulse")).toBeTruthy();
    expect(root.querySelectorAll(".main .stat.display").length).toBe(1);
    expect(root.querySelector(".main .tabs-inline")).toBeNull(); // no Summary/Segments tabs
    expect(root.querySelector(".main .card")).toBeNull(); // no card registry / customization wall

    // Breadcrumb-as-nav-stack: leaf is the page h1 + aria-current; zone root is the first crumb.
    expect(root.querySelector(".crumb-current")?.textContent).toContain("Pulse");
    expect(root.querySelector("h1.crumb-current[aria-current='page']")).toBeTruthy();
    expect(root.querySelector("nav[aria-label='Breadcrumb'] .crumb")?.textContent).toBe("Monitor"); // workflow zone root (Pulse ∈ Monitor)
    // Scoped workspace status bar — not an unrelated Today total.
    expect(root.querySelector(".statusbar")?.getAttribute("aria-label")).toBe("Workspace status");
  });

  it("carries live connection status in the persistent bottom tape, not the top bar", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());
    // Status lives in the tape: a dot (shape/color) + concise text (WCAG 1.4.1), always visible.
    const conn = root.querySelector(".statusbar .conn-status");
    expect(conn).toBeTruthy();
    expect(conn!.querySelector(".status-dot")).toBeTruthy();
    expect(conn!.querySelector(".conn-text")?.textContent).toMatch(/Local service (connected|retrying)|Connecting to local service/);
    // The top bar no longer carries the status.
    expect(root.querySelector(".topbar .conn-status")).toBeNull();
  });

  it("demotes the theme toggle out of the top bar and relocates the tagline to the sidebar footer", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());
    // No always-visible theme button in the top bar — theme is reached via ⌘K + Settings.
    expect(root.querySelector('.topbar [aria-label="Toggle theme"]')).toBeNull();
    // The brand is just the mark + name; the tagline is gone from the header.
    expect(root.querySelector(".brand .brand-sub")).toBeNull();
    expect(root.querySelector(".brand")?.textContent ?? "").not.toContain("cost profiler");
    // The tagline now lives quietly in the sidebar footer.
    expect(root.querySelector(".sidebar .nav-footer .nav-tagline")?.textContent).toContain(
      "Cost profiler for AI agents"
    );
  });

  it("status bar is scoped, never an unrelated Today total", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());
    // The topbar pill and the old Today/burn tape are gone.
    expect(root.querySelector(".topbar .spend-pill")).toBeNull();
    expect(root.querySelector(".tape-spend")).toBeNull();
    const statusbar = root.querySelector(".statusbar")!;
    expect(statusbar.getAttribute("aria-label")).toBe("Workspace status");
    // Shows capture/connection + freshness; no unrelated Today total or projected burn.
    expect(statusbar.querySelector(".conn-status")).toBeTruthy();
    expect(statusbar.textContent).not.toContain("Today");
    expect(statusbar.querySelector(".tape-burn")).toBeNull();
    // The scoped count+spend slot exists for workspaces to populate (empty by default).
    expect(statusbar.querySelector(".status-scope")).toBeTruthy();
  });

  it("keeps the header one continuous band and clamps the grid rows", () => {
    const css = readFileSync(resolve(process.cwd(), "src/ui/app.css"), "utf8");
    // The brand no longer carries a vertical seam (border-right) that boxed it off from the topbar.
    const brand = css.match(/(?:^|\n)\.brand\s*\{([^}]*)\}/);
    expect(brand, "expected a .brand rule").toBeTruthy();
    expect(brand![1]).not.toMatch(/border-right:|border-inline-end:/);
    expect(brand![1]).toMatch(/border-bottom/); // the shared band underline stays
    // The scroll panes clamp their 1fr grid track so a tall screen scrolls internally (no page-level
    // overflow that read as clipped content under the header).
    const main = css.match(/(?:^|\n)\.main\s*\{([^}]*)\}/);
    expect(main![1]).toMatch(/min-height:\s*0/);
    const sidebar = css.match(/(?:^|\n)\.sidebar\s*\{([^}]*)\}/);
    expect(sidebar![1]).toMatch(/min-height:\s*0/);
  });

  it("structures the top bar into lead/center/trail zones", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());
    const topbar = root.querySelector(".topbar")!;
    // Drag-region is macOS-gated now — asserted separately; here jsdom is non-mac so
    // the band is a plain toolbar (not a drag region).
    expect(topbar.hasAttribute("data-tauri-drag-region")).toBe(false);
    // Three explicit zones, no lone elastic spacer.
    expect(topbar.querySelector(".topbar-lead")).toBeTruthy();
    expect(topbar.querySelector(".topbar-center")).toBeTruthy();
    expect(topbar.querySelector(".topbar-trail")).toBeTruthy();
    expect(topbar.querySelector(":scope > .spacer")).toBeNull();
    // Content lands in the right zones: breadcrumb-nav + range in the lead. The topbar command button
    // is REMOVED — the rail Commands row is the single menu surface — and the
    // spend pill is gone.
    expect(topbar.querySelector(".topbar-lead .breadcrumb-nav")).toBeTruthy();
    expect(topbar.querySelector(".topbar-lead .range-control")).toBeTruthy();
    expect(topbar.querySelector(".topbar-trail .icon-btn")).toBeNull();
    expect(topbar.querySelector(".spend-pill")).toBeNull();
    // The command surface moved to the rail footer, above Capture (Connect).
    const commands = root.querySelector(".nav-footer .nav-commands");
    expect(commands).toBeTruthy();
    expect(commands?.getAttribute("data-action")).toBe("action:open-palette");
  });

  it("marks actual macOS titlebar click targets while Win/Linux remain non-draggable", async () => {
    // jsdom UA is not macOS and no data-os is stamped → the band must NOT be a drag region.
    location.hash = "";
    const nonMac = document.createElement("div");
    await mountApp(nonMac, new FakeClient());
    expect(nonMac.querySelector(".topbar[data-tauri-drag-region]")).toBeNull();
    expect(nonMac.querySelector(".brand[data-tauri-drag-region]")).toBeNull();
    expect(nonMac.querySelector(".topbar-center[data-tauri-drag-region]")).toBeNull();
    expect(nonMac.querySelector(".brand-name[data-tauri-drag-region]")).toBeNull();

    // Tauri 2.11 drag markers are self-targeted: actual SVG/text/flex click targets and the exposed
    // toolbar background must carry the marker. This makes the unified toolbar drag like a native
    // macOS app without marking interactive descendants.
    document.documentElement.dataset.os = "macos";
    try {
      const mac = document.createElement("div");
      await mountApp(mac, new FakeClient());
      expect(mac.querySelector(".topbar[data-tauri-drag-region]")).toBeTruthy();
      expect(mac.querySelector(".brand[data-tauri-drag-region]")).toBeTruthy();
      expect(mac.querySelector(".brand-mark[data-tauri-drag-region]")).toBeTruthy();
      expect(mac.querySelector(".brand-name[data-tauri-drag-region]")).toBeTruthy();
      expect(mac.querySelector(".topbar-lead[data-tauri-drag-region]")).toBeNull();
      expect(mac.querySelector(".topbar-center[data-tauri-drag-region]")).toBeTruthy();
      expect(mac.querySelector(".topbar-trail[data-tauri-drag-region]")).toBeNull();
      // Interactive descendants remain unmarked/clickable.
      expect(mac.querySelector(".topbar input[data-tauri-drag-region]")).toBeNull();
      expect(mac.querySelector(".topbar select[data-tauri-drag-region]")).toBeNull();
      expect(mac.querySelector(".topbar a[data-tauri-drag-region]")).toBeNull();
    } finally {
      delete document.documentElement.dataset.os;
    }
  });

  it("lands on Pulse and guides to capture when there is no attributable spend", async () => {
    location.hash = "";
    // Zero spend + a blind capture channel: Pulse must still render its statement and surface an
    // honest path to set up capture — never a fabricated $0 dashboard. Overview's legacy `.empty-state`
    // is retired with the screen; the capture guidance now lives in Pulse's attention queue.
    class Empty extends FakeClient {
      async listRuns() {
        return [];
      }
      async today() {
        return { total_micros: 0, pricing_version: "x", effective_date: "2026-06-01" };
      }
      async coverage() {
        return { status: "amber" as const, has_proxy: false, has_otel: false, blind_sources: ["codex"], sources: [] };
      }
      async burnrate(): ReturnType<TareClient["burnrate"]> {
        return {
          run_rate_micros_per_day: 0,
          effective_rate_micros_per_day: 0,
          spent_micros: 0,
          active_days: 0,
          daily_spend_micros: [],
          days_elapsed: 0,
          days_in_period: 30,
          projected_micros: 0,
          projected_low_micros: 0,
          projected_high_micros: 0,
          cap_micros: 0,
          on_track: true,
          headroom_days: null,
          period: "month",
          period_start: "2026-07-01",
          as_of: "2026-07-01",
          period_end: "2026-07-30",
        };
      }
      async budget() {
        return { period: "month", spent_micros: 0, cap_micros: 0, warn_pct: 80, pct: 0, status: "ok" };
      }
      async savings() {
        return { opportunities: [], total_recoverable_micros: 0, total_spend_micros: 0, savings_index: 100, pricing_version: "x", estimated: true };
      }
    }
    const root = document.createElement("div");
    await mountApp(root, new Empty());
    // Pulse renders (not the old Overview empty-state), and the blind-capture channel floats an honest
    // capture row — uncaptured spend, never a fabricated $0.
    expect(root.querySelector(".main .pulse")).toBeTruthy();
    const capture = root.querySelector(".main .attn-capture");
    expect(capture?.textContent).toMatch(/missing from the totals, not zero/);
    expect(capture?.querySelector(".pulse-attn-dollars")?.textContent).toBe("uncaptured");
  });

  it("renders a visible error (not a blank screen) when the client fails", async () => {
    location.hash = "";
    class Broken extends TestTareClient implements TareClient {
      async listRuns(): Promise<string[]> {
        throw new Error("backend down");
      }
      async flamegraph(): Promise<FlamegraphModel> {
        throw new Error("x");
      }
      async report(): Promise<Report> {
        throw new Error("x");
      }
      async trend(): Promise<TrendReport> {
        throw new Error("x");
      }
      async anomalies(): Promise<Anomaly[]> {
        throw new Error("x");
      }
      async rollup(): Promise<never> {
        throw new Error("x");
      }
      async vendorToday(): Promise<never> {
        throw new Error("x");
      }
      async reconcile(): Promise<never> {
        throw new Error("x");
      }
      async coverage(): Promise<never> {
        throw new Error("x");
      }
      async effectiveness(): Promise<never> {
        throw new Error("x");
      }
      async confidence(): Promise<never> {
        throw new Error("x");
      }
      async today(): Promise<never> {
        throw new Error("x");
      }
      async runStatus(): Promise<never> {
        throw new Error("x");
      }
      async explain(): Promise<never> {
        throw new Error("x");
      }
    }
    const root = document.createElement("div");
    await mountApp(root, new Broken());
    expect(root.querySelector(".main .error")).toBeTruthy();
  });

  it("activates the nav + updates the toolbar title when routing", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());
    location.hash = "#/investigate";
    // hashchange is synchronous in jsdom; allow the async screen render to settle.
    await new Promise((r) => setTimeout(r, 0));
    // The canonical Investigate workspace had NO rail entry, so the sidebar showed nothing active
    // across the whole workspace — including Run Profile and Compare.
    expect(root.querySelector(".sidebar .nav-item.active")?.textContent).toBe("Investigate");
    expect(root.querySelector(".crumb-current")?.textContent).toContain("Investigate");
  });

  // Compatibility links resolve all the way to their canonical workspace before breadcrumb paint.
  it("labels legacy links with their canonical workspace instead of a raw route id", async () => {
    for (const [hash, expected] of [
      ["#/trends", "Investigate"],
      ["#/experiments", "Optimize"],
      ["#/units", "Investigate"],
    ] as const) {
      location.hash = "";
      const root = document.createElement("div");
      await mountApp(root, new FakeClient());
      location.hash = hash;
      await new Promise((r) => setTimeout(r, 0));
      const crumb = root.querySelector(".crumb-current")?.textContent ?? "";
      expect(crumb, `${hash} breadcrumb`).toContain(expected);
    }
  });

  it("shows a loud capture banner only when the active profile is max_inspect", async () => {
    // Default profile → the banner stays hidden.
    location.hash = "";
    const plain = document.createElement("div");
    await mountApp(plain, new FakeClient());
    await new Promise((r) => setTimeout(r, 0));
    const plainBanner = plain.querySelector(".capture-banner");
    expect(plainBanner?.hasAttribute("hidden") ?? true).toBe(true);

    // max_inspect → the banner is shown and links to Settings to review/purge.
    class Inspecting extends FakeClient {
      async config() {
        return { privacy: { profile: "max_inspect" } } as unknown as import("../src/client.js").TareConfigDto;
      }
    }
    const root = document.createElement("div");
    await mountApp(root, new Inspecting());
    await new Promise((r) => setTimeout(r, 0));
    const banner = root.querySelector(".capture-banner") as HTMLAnchorElement;
    expect(banner.hasAttribute("hidden")).toBe(false);
    expect(banner.textContent).toContain("Capturing redacted bodies");
    expect(banner.getAttribute("href")).toBe("#/settings");
  });

  it("fills the toolbar contextual-action slot per screen and clears it on navigation", async () => {
    location.hash = "";
    const root = document.createElement("div");
    await mountApp(root, new FakeClient());
    location.hash = "#/runs";
    await new Promise((r) => setTimeout(r, 0));
    const action = root.querySelector(".topbar-actions .topbar-action") as HTMLAnchorElement;
    expect(action?.textContent).toBe("Compare runs");
    expect(action?.getAttribute("href")).toMatch(/^#\/investigate\/compare\?/);
    expect(action?.getAttribute("href")).toContain("entity=runs");
    // Navigating to a screen with no action clears the slot (no stale action lingers).
    location.hash = "#/pulse";
    await new Promise((r) => setTimeout(r, 0));
    expect(root.querySelector(".topbar-actions .topbar-action")).toBeNull();
  });
});
