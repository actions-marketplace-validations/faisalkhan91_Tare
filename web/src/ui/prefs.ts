// App preferences (UI-only): refresh cadence + first-run flag, persisted in localStorage so they
// work identically in the browser and the Tauri WebView. Capture config (budget/privacy/...) is
// separate and lives in tare.toml. Theme has its own module (theme.ts).

const RK = "tare-refresh-ms";
const OK = "tare-onboarded";

function store(): Storage | null {
  try {
    return typeof localStorage !== "undefined" ? localStorage : null;
  } catch {
    return null;
  }
}

export function refreshMs(): number {
  const v = Number(store()?.getItem(RK));
  return Number.isFinite(v) && v >= 1000 ? v : 3000;
}
export function setRefreshMs(ms: number): void {
  store()?.setItem(RK, String(Math.max(1000, Math.trunc(ms))));
}

export function isOnboarded(): boolean {
  return store()?.getItem(OK) === "1";
}
export function setOnboarded(v: boolean): void {
  if (v) store()?.setItem(OK, "1");
  else store()?.removeItem(OK);
}

// Which onboarding step a not-yet-onboarded user reached, so an un-wired returning user resumes
// at Connect instead of restarting. Cleared implicitly once onboarded.
const OBSTEP = "tare-onboard-step";
export function onboardStep(): number {
  const n = Number(store()?.getItem(OBSTEP) ?? "0");
  return Number.isInteger(n) && n >= 0 ? n : 0;
}
export function setOnboardStep(n: number): void {
  try {
    store()?.setItem(OBSTEP, String(n));
  } catch {
    /* ignore */
  }
}

// ---- provider preset (which provider the user points at Tare) ----
// UI-only: lets onboarding's pick pre-select the Capture utility's provider. The id is validated
// against PROVIDER_PRESETS at the call site, so an unknown/legacy value falls back to Anthropic.
const PK = "tare-provider";
export function providerPref(): string {
  return store()?.getItem(PK) ?? "anthropic";
}
export function setProviderPref(id: string): void {
  try {
    store()?.setItem(PK, id);
  } catch {
    /* ignore */
  }
}

// ---- runs workbench: pinned runs + baseline ----
// Pinned run ids float above the rest of the Runs table; one baseline run drives a "Δ vs baseline"
// column. All UI-only, localStorage-persisted (single-user, local-first).
const PINK = "tare-pinned-runs";
const BASEK = "tare-baseline-run";
const BDLTK = "tare-show-baseline-delta";

export function pinnedRuns(): string[] {
  try {
    const v = JSON.parse(store()?.getItem(PINK) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}
export function baselineRun(): string | null {
  return store()?.getItem(BASEK) || null;
}
/// Set (or clear, with null) the single baseline run. Setting the current baseline again clears it.
export function setBaselineRun(id: string | null): void {
  try {
    if (id) store()?.setItem(BASEK, id);
    else store()?.removeItem(BASEK);
  } catch {
    /* ignore */
  }
}
export function showBaselineDelta(): boolean {
  return store()?.getItem(BDLTK) !== "0"; // default on
}
export function setShowBaselineDelta(v: boolean): void {
  try {
    store()?.setItem(BDLTK, v ? "1" : "0");
  } catch {
    /* ignore */
  }
}

// ---- recently-opened runs ----
// An LRU of opened run ids so the handful of opaque-id runs a user touches are re-findable in the
// sidebar/palette. Pinning reuses the pinned-runs set — one "pinned" concept.
const RECENTK = "tare-recent-runs";
const RECENT_CAP = 10;

export function recentRuns(): string[] {
  try {
    const v = JSON.parse(store()?.getItem(RECENTK) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string").slice(0, RECENT_CAP) : [];
  } catch {
    return [];
  }
}
/// Record an opened run id at the front of the LRU (dedup, capped).
export function recordOpenedRun(id: string): void {
  const next = [id, ...recentRuns().filter((r) => r !== id)].slice(0, RECENT_CAP);
  try {
    store()?.setItem(RECENTK, JSON.stringify(next));
  } catch {
    /* ignore */
  }
}

function writeRunIds(key: string, ids: string[]): void {
  try {
    store()?.setItem(key, JSON.stringify(ids));
  } catch {
    /* ignore */
  }
}

/**
 * Remove one run from every UI-only reference that can expose it as a destination. Runs can
 * legitimately disappear when a store is purged or the desktop app is pointed at another DB; an
 * old localStorage LRU/pin must not keep routing to a record that the active store cannot load.
 */
export function forgetRunReferences(id: string): void {
  writeRunIds(RECENTK, recentRuns().filter((runId) => runId !== id));
  writeRunIds(PINK, pinnedRuns().filter((runId) => runId !== id));
  if (baselineRun() === id) setBaselineRun(null);
}

/** Reconcile persisted run destinations with an authoritative run-id list from the active store. */
export function reconcileRunReferences(validRunIds: readonly string[]): void {
  const valid = new Set(validRunIds);
  writeRunIds(RECENTK, [...new Set(recentRuns())].filter((id) => valid.has(id)));
  writeRunIds(PINK, [...new Set(pinnedRuns())].filter((id) => valid.has(id)));
  // Baseline B is analytical context, not a navigation destination. It may intentionally describe a
  // run outside a narrowed/fixture-backed current list, so only forgetRunReferences() may clear it
  // after a direct lookup proves that exact run is gone.
}

// ---- global analysis range ----
// A workspace-level time window the Analyze screens share. localStorage so it survives navigation
// (route-query would be dropped by the sidebar/breadcrumb links) and is read synchronously at
// render time. A saved investigation serializes its own scope; this is the ambient default.
import type { RangeKey, RangePref } from "./range.js";
const RNG = "tare-range";

export function rangePref(): RangePref {
  const raw = store()?.getItem(RNG);
  if (!raw) return { key: "30d" };
  try {
    const v = JSON.parse(raw) as Partial<RangePref>;
    const key: RangeKey =
      v.key === "7d" || v.key === "90d" || v.key === "custom" ? v.key : "30d";
    return { key, from: typeof v.from === "string" ? v.from : undefined, to: typeof v.to === "string" ? v.to : undefined };
  } catch {
    return { key: "30d" };
  }
}
export function setRangePref(p: RangePref): void {
  try {
    store()?.setItem(RNG, JSON.stringify(p));
  } catch {
    /* ignore */
  }
}

// Default landing screen: which screen opens on launch when no deep-link hash is
// present. A preference that fits the register (unlike the removed rearrangeable card wall). Stored
// values may be legacy screen names; migrateDefaultScreen (routes.ts) maps them to canonical workspaces.
const DS = "tare-default-screen";
const DEFAULT_SCREENS = ["pulse", "investigate", "optimize"];
export function defaultScreen(): string {
  const v = store()?.getItem(DS);
  if (v === "live" || v === "overview") return "pulse";
  if (v === "runs" || v === "sessions" || v === "trends") return "investigate";
  if (v === "experiments") return "optimize";
  return v && DEFAULT_SCREENS.includes(v) ? v : "pulse";
}
export function setDefaultScreen(s: string): void {
  try {
    if (DEFAULT_SCREENS.includes(s)) store()?.setItem(DS, s);
  } catch {
    /* ignore */
  }
}

// ---- density (comfortable | compact) ----
const DK = "tare-density";
export type Density = "comfortable" | "compact";

export function density(): Density {
  return store()?.getItem(DK) === "compact" ? "compact" : "comfortable";
}
export function setDensity(d: Density): void {
  try {
    store()?.setItem(DK, d);
  } catch {
    /* ignore */
  }
}
export function applyDensity(d: Density, doc: Document = document): void {
  doc.documentElement.setAttribute("data-density", d);
}
