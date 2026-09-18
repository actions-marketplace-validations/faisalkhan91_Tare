// Desktop `__TAURI__` stub for the served-twin Playwright project (tare desktop-testing layer).
//
// macOS has no WKWebView WebDriver, so we cannot drive the real embedded WebView. Instead we load the
// SAME built assets the WebView loads (index.tauri.html + dist + desktop.css) in headless Chromium and
// stand in a hand-rolled `window.__TAURI__` so `bootTauri.ts` mounts the desktop client path. The
// stub's `invoke` rides the existing `installFixtures` route interception (page.route("**/__tare/**"))
// via fetch, so there is ONE fixture source and no divergence.
//
// DESKTOP_COMMANDS is the IPC seam contract: for each command it records the HTTP fixture path (from
// httpClient.ts) and the return SHAPE tauriClient.ts expects (from tauriClient.ts). tauriClient's `j<T>`
// helper does `JSON.parse(await invoke(...))`, so a "json" command MUST return a JSON *string* while an
// "object"/"const" command returns a live value — get the classification wrong and the desktop app
// throws at runtime. `tauriStubContract.test.ts` (vitest) cross-checks these keys against BOTH
// tauriClient's invoke() literals AND gui.rs's generate_handler! registry, so this table cannot
// silently drift from either side of the seam.

import type { Page } from "@playwright/test";

/// A command's fixture route + the return shape tauriClient.ts expects.
///  - "json"    : GET; fixture returns JSON; tauriClient `j<T>()` parses it → return the response TEXT.
///  - "object"  : GET; tauriClient uses the value directly → return the PARSED object.
///  - "rawText" : GET; tauriClient expects a raw (non-JSON) string → return the response text.
///  - "rawExplain": GET /explain; tauriClient expects the narrative string → return `.explain`.
///  - "post"    : POST the request body; tauriClient `j<T>()` parses it → return the response TEXT.
///  - "postVoid": POST (write); tauriClient discards the result → return null.
///  - "const"   : desktop-only command with no HTTP twin → return `value` verbatim (already a JSON
///                string for `j<T>` commands, or a live value otherwise).
export interface CommandSpec {
  path?: string;
  method?: "GET" | "POST";
  ret: "json" | "object" | "rawText" | "rawExplain" | "post" | "postVoid" | "const";
  value?: unknown;
}

export const DESKTOP_COMMANDS: Record<string, CommandSpec> = {
  // Object-returning reads (tauriClient uses the value directly, no JSON.parse).
  today_spend: { path: "/__tare/today", ret: "object" },
  list_runs: { path: "/__tare/runs", ret: "object" },
  run_flamegraph: { path: "/__tare/flamegraph", ret: "object" },
  run_profile: { path: "/__tare/profile", ret: "object" },
  cost_frontier: { path: "/__tare/frontier", ret: "object" },
  // JSON-string reads (tauriClient `j<T>()` JSON.parses the returned string).
  report: { path: "/__tare/report", ret: "json" },
  trend: { path: "/__tare/trend", ret: "json" },
  anomalies: { path: "/__tare/anomalies", ret: "json" },
  cost_regressions: { path: "/__tare/cost_regressions", ret: "json" },
  run_status: { path: "/__tare/run_status", ret: "json" },
  run_statuses: { path: "/__tare/run_statuses", ret: "json" },
  run_meta: { path: "/__tare/run_meta", ret: "json" },
  run_steps: { path: "/__tare/run_steps", ret: "json" },
  transcript: { path: "/__tare/transcript", ret: "json" },
  recent_steps: { path: "/__tare/recent_steps", ret: "json" },
  session_autopsy: { path: "/__tare/session_autopsy", ret: "json" },
  advise: { path: "/__tare/advise", ret: "json" },
  savings: { path: "/__tare/savings", ret: "json" },
  action_plan: { path: "/__tare/action_plan", ret: "json" },
  cache_ledger: { path: "/__tare/cache_ledger", ret: "json" },
  reasoning: { path: "/__tare/reasoning", ret: "json" },
  effectiveness: { path: "/__tare/effectiveness", ret: "json" },
  confidence: { path: "/__tare/confidence", ret: "json" },
  whatif: { path: "/__tare/whatif", ret: "json" },
  diff: { path: "/__tare/diff", ret: "json" },
  flame_diff: { path: "/__tare/flame_diff", ret: "json" },
  pricing: { path: "/__tare/pricing", ret: "json" },
  receipt: { path: "/__tare/receipt", ret: "json" },
  rollup: { path: "/__tare/rollup", ret: "json" },
  punchcard: { path: "/__tare/punchcard", ret: "json" },
  heatmap: { path: "/__tare/heatmap", ret: "json" },
  sessions: { path: "/__tare/sessions", ret: "json" },
  correlate: { path: "/__tare/correlate", ret: "json" },
  lineages: { path: "/__tare/lineage", ret: "json" },
  units: { path: "/__tare/units", ret: "json" },
  sessions_live: { path: "/__tare/sessions_live", ret: "json" },
  vendor_today: { path: "/__tare/vendor_today", ret: "json" },
  burnrate: { path: "/__tare/burnrate", ret: "json" },
  coverage: { path: "/__tare/coverage", ret: "json" },
  reconcile: { path: "/__tare/reconcile", ret: "json" },
  loops: { path: "/__tare/loops", ret: "json" },
  failures: { path: "/__tare/failures", ret: "json" },
  lenses: { path: "/__tare/lenses", ret: "json" },
  budget: { path: "/__tare/budget", ret: "json" },
  sandwich: { path: "/__tare/sandwich", ret: "json" },
  otlp_status: { path: "/__tare/otlp_status", ret: "json" },
  get_config: { path: "/__tare/config", ret: "json" },
  run_note: { path: "/__tare/run_note", ret: "json" },
  runs_by_tag: { path: "/__tare/notes_by_tag", ret: "json" },
  starred_runs: { path: "/__tare/starred_runs", ret: "json" },
  list_investigations: { path: "/__tare/investigations", ret: "json" },
  savings_actions: { path: "/__tare/savings/actions", ret: "json" },
  // Raw-string reads.
  explain: { path: "/__tare/explain", ret: "rawExplain" },
  export: { path: "/__tare/export", ret: "rawText" },
  // JSON-string POSTs (typed request body → parsed AnalysisResponse/result envelope).
  cohort_resolve: { path: "/__tare/cohort/resolve", ret: "post" },
  cohort_facets: { path: "/__tare/cohort/facets", ret: "post" },
  cohort_compare: { path: "/__tare/cohort/compare", ret: "post" },
  cohort_search: { path: "/__tare/cohort/search", ret: "post" },
  cohort_timeline: { path: "/__tare/cohort/timeline", ret: "post" },
  anomaly_why: { path: "/__tare/anomaly_why", ret: "post" },
  experiment: { path: "/__tare/experiment", ret: "post" },
  savings_verify: { path: "/__tare/savings/verify", ret: "post" },
  // Write POSTs (tauriClient discards the result).
  save_run_note: { path: "/__tare/notes", ret: "postVoid" },
  delete_run_note: { path: "/__tare/notes/delete", ret: "postVoid" },
  set_run_quality: { path: "/__tare/quality", ret: "postVoid" },
  acknowledge_anomaly: { path: "/__tare/acknowledge", ret: "postVoid" },
  transcript_purge: { path: "/__tare/transcript_purge", ret: "postVoid" },
  save_investigation: { path: "/__tare/investigations", ret: "postVoid" },
  delete_investigation: { path: "/__tare/investigations/delete", ret: "postVoid" },
  savings_accept: { path: "/__tare/savings/accept", ret: "postVoid" },
  savings_dismiss: { path: "/__tare/savings/dismiss", ret: "postVoid" },
  savings_unaccept: { path: "/__tare/savings/unaccept", ret: "postVoid" },
  // Desktop-only commands with no HTTP twin. `j<T>` commands (proxy_*/config_origins) return a JSON
  // STRING; the rest return live values. seed_demo mirrors tauriClient's string-or-"demo" fallback.
  proxy_status: { ret: "const", value: JSON.stringify({ running: true, port: 0, url: "" }) },
  start_proxy: { ret: "const", value: JSON.stringify({ running: true, port: 0, url: "" }) },
  stop_proxy: { ret: "const", value: JSON.stringify({ running: false, port: 0, url: "" }) },
  config_origins: { ret: "const", value: "{}" },
  get_background_on_close: { ret: "const", value: false },
  set_background_on_close: { ret: "const", value: null },
  save_config: { ret: "const", value: null },
  notify: { ret: "const", value: null },
  seed_demo: { ret: "const", value: "demo" },
};

/// Options for the injected stub. `reservePx` mirrors the native init-script's ShellInit snapshot
/// (gui.rs `shell_init_script`), whose macOS fallback is 96px; override to prove the measured branch.
export interface TauriStubOptions {
  os?: "macos" | "windows" | "linux";
  material?: "vibrancy" | "mica" | "flat";
  reservePx?: number;
}

/// Inject a `window.__TAURI__` stub BEFORE any page script runs, so `index.tauri.html`'s desktop boot
/// path (`bootTauri.ts`) mounts the invoke client + desktop chrome. Call before `page.goto`, alongside
/// `installFixtures(page)` (whose route interception this stub's `invoke` fetches through). Exposes two
/// test hooks in page context: `window.__tauriEmit(event, payload)` fires a native event (navigate /
/// open-palette / find / system-accent), and `window.__tauriWindow.{setFocus,setFullscreen}` drive the
/// focus/fullscreen window signals.
export async function installTauriStub(page: Page, options: TauriStubOptions = {}): Promise<void> {
  const os = options.os ?? "macos";
  const material = options.material ?? (os === "macos" ? "vibrancy" : os === "windows" ? "mica" : "flat");
  const reservePx = options.reservePx ?? 96;
  await page.addInitScript(
    (cfg: { commands: Record<string, CommandSpec>; os: string; material: string; reservePx: number }) => {
      const { commands, os, material, reservePx } = cfg;
      // NB: Playwright's addInitScript runs at document-start, BEFORE <html> is parsed, so
      // document.documentElement can be null here — set the globals first (they persist regardless of
      // parse timing) and stamp the DOM best-effort. data-os/data-material are guaranteed downstream
      // anyway: applyOsClass resolves the OS from the macOS userAgent this project pins, and bootTauri
      // sets the material from it — so the early stamp just matches the native init-script's no-flash
      // behavior, it is not load-bearing.
      (globalThis as Record<string, unknown>).__TARE_SHELL_INIT__ = {
        os,
        material,
        traffic_light_reserve_px: reservePx,
      };
      const stamp = (): void => {
        const r = document.documentElement;
        if (!r) return;
        if (!r.dataset.os) r.dataset.os = os;
        if (!r.dataset.material) r.dataset.material = material;
      };
      stamp();
      if (!document.documentElement) {
        // <html> not parsed yet — stamp the instant it appears (before the deferred boot module reads it).
        const obs = new MutationObserver(() => {
          if (document.documentElement) {
            stamp();
            obs.disconnect();
          }
        });
        obs.observe(document, { childList: true, subtree: true });
      }

      // Native event registry + a test hook to fire events the Rust shell would emit.
      const listeners: Record<string, Array<(e: { payload: unknown }) => void>> = {};
      (globalThis as Record<string, unknown>).__tauriEmit = (event: string, payload?: unknown): void => {
        for (const cb of listeners[event] ?? []) cb({ payload });
      };
      const listen = (event: string, cb: (e: { payload: unknown }) => void): Promise<() => void> => {
        (listeners[event] ??= []).push(cb);
        return Promise.resolve(() => {
          listeners[event] = (listeners[event] ?? []).filter((c) => c !== cb);
        });
      };

      // Window API mock (focus/fullscreen) + test hooks to drive them.
      let fullscreen = false;
      let focusCb: ((e: { payload: boolean }) => void) | null = null;
      let resizeCb: (() => void) | null = null;
      const currentWindow = {
        onFocusChanged: (cb: (e: { payload: boolean }) => void) => {
          focusCb = cb;
          return Promise.resolve(() => {});
        },
        onResized: (cb: () => void) => {
          resizeCb = cb;
          return Promise.resolve(() => {});
        },
        isFullscreen: () => Promise.resolve(fullscreen),
      };
      (globalThis as Record<string, unknown>).__tauriWindow = {
        setFocus: (focused: boolean) => focusCb?.({ payload: focused }),
        setFullscreen: (fs: boolean) => {
          fullscreen = fs;
          resizeCb?.();
        },
      };

      const invoke = async (cmd: string, args: Record<string, unknown> = {}): Promise<unknown> => {
        const spec = commands[cmd];
        if (!spec) throw new Error(`tauri stub: unknown command "${cmd}"`);
        if (spec.ret === "const") return spec.value;
        const method = spec.method ?? (spec.ret === "post" || spec.ret === "postVoid" ? "POST" : "GET");
        const init: RequestInit = { method };
        if (method === "POST") {
          // tauriClient sends typed requests as a stringified `body` arg; other writes send the raw
          // args. Ride the same fixture route either way.
          init.headers = { "content-type": "application/json" };
          init.body = typeof args.body === "string" ? args.body : JSON.stringify(args);
        }
        const url = cmd === "burnrate" && typeof args.range === "string"
          ? `${spec.path}?range=${encodeURIComponent(args.range)}`
          : spec.path as string;
        const res = await fetch(url, init);
        const text = await res.text();
        if (spec.ret === "object") return JSON.parse(text);
        if (spec.ret === "rawText") return text;
        if (spec.ret === "rawExplain") {
          try {
            return (JSON.parse(text) as { explain?: string }).explain ?? "";
          } catch {
            return "";
          }
        }
        if (spec.ret === "postVoid") return null;
        // "json" and "post": tauriClient JSON.parses the returned string, so hand back the raw text.
        return text;
      };

      (globalThis as Record<string, unknown>).__TAURI__ = {
        core: { invoke },
        event: { listen },
        window: { getCurrentWindow: () => currentWindow, getCurrent: () => currentWindow },
      };
    },
    { commands: DESKTOP_COMMANDS, os, material, reservePx }
  );
}
