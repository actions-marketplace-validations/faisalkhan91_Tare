// A real TareClient backed by the loopback read API that `tare serve` exposes
// (GET /__tare/{runs,flamegraph,report,trend}). The browser viewer uses this; the desktop
// build can instead provide a Tauri-`invoke` client. All data is the Rust core's — estimated,
// counts only.

import type { FlamegraphModel } from "./svg.js";
import type { TrendReport } from "./trendSvg.js";
import type {
  Anomaly,
  AnomalyWhy,
  AnomalyWhyRequest,
  CacheAdvice,
  CostRegression,
  FlameDiffModel,
  OtlpStatus,
  PricingInfo,
  ReceiptResult,
  FailureWasteReport,
  Lenses,
  PeriodBudget,
  Sandwich,
  LoopWasteReport,
  ReportDiff,
  RollupReport,
  PunchcardModel,
  HeatmapModel,
  Effectiveness,
  EstimateConfidence,
  SavingsLedger,
  ActionPlan,
  CacheLedger,
  ReasoningBreakout,
  Report,
  ProfileSort,
  ProfileTable,
  RunStatus,
  RunMeta,
  RunNote,
  RunStep,
  TranscriptStep,
  RecentStep,
  SessionLive,
  SessionReport,
  CorrelationReport,
  LineageReport,
  UnitReport,
  TareClient,
  TareConfigDto,
  TodaySpend,
  TrendQuery,
  VendorToday,
  BurnRate,
  BurnRateRange,
  Coverage,
  Reconciliation,
  WhatIfRecommendations,
  SessionAutopsy,
} from "./client.js";
import type {
  AnalysisResponse,
  CohortCompareRequest,
  CohortCompareResult,
  CohortFacetRequest,
  CohortFacetResult,
  CohortResolveResult,
  CohortSearchRequest,
  CohortSearchResult,
  CohortSpec,
  CohortTimelineRequest,
  CohortTimelineResult,
  ExperimentRequest,
  ExperimentResult,
  Frontier,
  SavingsAction,
  SavingsActionIdentity,
  SavingsActionRequest,
  SavingsVerifyRequest,
  SavingsVerifyResult,
} from "./analysis/types.js";
import type { SavedInvestigationV2 } from "./analysis/investigation.js";
import { upgradeInvestigationEntityRefs } from "./analysis/investigation.js";

export function createHttpClient(base = ""): TareClient {
  async function getJson<T>(path: string): Promise<T> {
    const res = await fetch(`${base}${path}`);
    if (!res.ok) {
      throw new Error(`tare read API ${path}: HTTP ${res.status}`);
    }
    return (await res.json()) as T;
  }
  // Loopback POST — the write API (run notes). Same-origin as the served UI.
  async function postJson<T>(path: string, body: unknown): Promise<T> {
    const res = await fetch(`${base}${path}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      throw new Error(`tare write API ${path}: HTTP ${res.status}`);
    }
    return (await res.json()) as T;
  }
  return {
    listRuns: () => getJson<string[]>("/__tare/runs"),
    flamegraph: (runId: string) =>
      getJson<FlamegraphModel>(`/__tare/flamegraph?run=${encodeURIComponent(runId)}`),
    sessionAutopsy: (runId: string, median?: number) => {
      const m = median != null ? `&median=${Math.trunc(median)}` : "";
      return getJson<SessionAutopsy>(`/__tare/session_autopsy?id=${encodeURIComponent(runId)}${m}`);
    },
    profile: (runId: string, sort?: ProfileSort, topN?: number) => {
      const p = new URLSearchParams({ run: runId });
      if (sort) p.set("sort", sort);
      if (topN != null) p.set("top", String(topN));
      return getJson<ProfileTable>(`/__tare/profile?${p.toString()}`);
    },
    frontier: () => getJson<Frontier>("/__tare/frontier"),
    saveQuality: (runId: string, score: number, source = "cli") =>
      postJson<{ ok: boolean }>("/__tare/quality", { run_id: runId, score, source }).then(() => undefined),
    report: () => getJson<Report>("/__tare/report"),
    trend: (q: TrendQuery) => {
      const qs = trendQs(q);
      return getJson<TrendReport>(`/__tare/trend${qs ? `?${qs}` : ""}`);
    },
    anomalies: (q: TrendQuery = {}) => {
      const qs = trendQs(q);
      return getJson<Anomaly[]>(`/__tare/anomalies${qs ? `?${qs}` : ""}`);
    },
    costRegressions: (q: { window?: number; threshold?: number } = {}) => {
      const p = new URLSearchParams();
      if (q.window != null) p.set("window", String(q.window));
      if (q.threshold != null) p.set("threshold", String(q.threshold));
      const qs = p.toString();
      return getJson<CostRegression[]>(`/__tare/cost_regressions${qs ? `?${qs}` : ""}`);
    },
    today: () => getJson<TodaySpend>("/__tare/today"),
    runStatus: (runId: string) =>
      getJson<RunStatus>(`/__tare/run_status?run=${encodeURIComponent(runId)}`),
    runStatuses: () => getJson<RunStatus[]>("/__tare/run_statuses"),
    runMeta: (runId: string) =>
      getJson<RunMeta>(`/__tare/run_meta?run=${encodeURIComponent(runId)}`),
    runSteps: (runId: string) =>
      getJson<RunStep[]>(`/__tare/run_steps?run=${encodeURIComponent(runId)}`),
    transcript: (runId: string, step: number) =>
      getJson<TranscriptStep | null>(
        `/__tare/transcript?run=${encodeURIComponent(runId)}&step=${step}`,
      ),
    recentSteps: (n?: number) =>
      getJson<RecentStep[]>(`/__tare/recent_steps${n ? `?n=${n}` : ""}`),
    explain: (runId: string) =>
      getJson<{ explain: string }>(`/__tare/explain?run=${encodeURIComponent(runId)}`).then(
        (r) => r.explain
      ),
    advise: () => getJson<CacheAdvice[]>("/__tare/advise"),
    savings: () => getJson<SavingsLedger>("/__tare/savings"),
    actionPlan: () => getJson<ActionPlan>("/__tare/action_plan"),
    cacheLedger: () => getJson<CacheLedger>("/__tare/cache_ledger"),
    reasoning: () => getJson<ReasoningBreakout>("/__tare/reasoning"),
    effectiveness: () => getJson<Effectiveness>("/__tare/effectiveness"),
    confidence: () => getJson<EstimateConfidence>("/__tare/confidence"),
    whatif: (crossProvider: boolean) =>
      getJson<WhatIfRecommendations>(
        `/__tare/whatif${crossProvider ? "?cross_provider=1" : ""}`
      ),
    diff: (a: string, b: string) =>
      getJson<ReportDiff>(
        `/__tare/diff?a=${encodeURIComponent(a)}&b=${encodeURIComponent(b)}`
      ),
    flameDiff: (a: string, b: string, normalized = false) =>
      getJson<FlameDiffModel>(
        `/__tare/flame_diff?a=${encodeURIComponent(a)}&b=${encodeURIComponent(b)}&normalized=${normalized}`
      ),
    pricing: () => getJson<PricingInfo>("/__tare/pricing"),
    receipt: (runId: string, maxPrivate = false) =>
      getJson<ReceiptResult>(
        `/__tare/receipt?run=${encodeURIComponent(runId)}${
          maxPrivate ? "&profile=max_private" : ""
        }`
      ),
    rollup: (by: string, filter?: { by: string; label: string }) =>
      getJson<RollupReport>(
        `/__tare/rollup?by=${encodeURIComponent(by)}` +
          (filter
            ? `&filter_by=${encodeURIComponent(filter.by)}&filter=${encodeURIComponent(filter.label)}`
            : "")
      ),
    punchcard: () => getJson<PunchcardModel>("/__tare/punchcard"),
    heatmap: () => getJson<HeatmapModel>("/__tare/heatmap"),
    sessions: () => getJson<SessionReport>("/__tare/sessions"),
    correlate: () => getJson<CorrelationReport>("/__tare/correlate"),
    lineages: () => getJson<LineageReport[]>("/__tare/lineage"),
    units: () => getJson<UnitReport>("/__tare/units"),
    sessionsLive: () => getJson<SessionLive[]>("/__tare/sessions_live"),
    vendorToday: () => getJson<VendorToday>("/__tare/vendor_today"),
    burnrate: (range?: BurnRateRange) =>
      getJson<BurnRate>(`/__tare/burnrate${range ? `?range=${encodeURIComponent(range)}` : ""}`),
    coverage: () => getJson<Coverage>("/__tare/coverage"),
    reconcile: (day?: string) =>
      getJson<Reconciliation>(`/__tare/reconcile${day ? `?day=${encodeURIComponent(day)}` : ""}`),
    loops: () => getJson<LoopWasteReport>("/__tare/loops"),
    failures: () => getJson<FailureWasteReport>("/__tare/failures"),
    lenses: () => getJson<Lenses>("/__tare/lenses"),
    budget: () => getJson<PeriodBudget>("/__tare/budget"),
    sandwich: (component: string) => getJson<Sandwich>(`/__tare/sandwich?component=${encodeURIComponent(component)}`),
    exportRun: async (runId: string, format: string) => {
      const res = await fetch(
        `${base}/__tare/export?run=${encodeURIComponent(runId)}&format=${encodeURIComponent(format)}`
      );
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      return res.text();
    },
    config: () => getJson<TareConfigDto>("/__tare/config"),
    saveConfig: () =>
      Promise.reject(
        new Error(
          "Editing capture settings needs the desktop app (or edit tare.toml directly). App preferences still work here."
        )
      ),
    canSaveConfig: () => false,
    // The browser app is already served by a running proxy; it doesn't control one.
    canControlProxy: () => false,
    proxyStatus: () => Promise.resolve({ running: true, port: 0, url: base || "" }),
    proxyStart: () => Promise.reject(new Error("proxy control is desktop-only")),
    proxyStop: () => Promise.reject(new Error("proxy control is desktop-only")),
    // OS notifications are a desktop affordance; the browser uses in-app toasts only.
    canNotify: () => false,
    notify: () => Promise.resolve(),
    // The browser has no window-close-to-tray; the toggle is hidden and writes are no-ops.
    canBackgroundOnClose: () => false,
    getBackgroundOnClose: () => Promise.resolve(false),
    setBackgroundOnClose: () => Promise.resolve(),
    otlpStatus: () => getJson<OtlpStatus>("/__tare/otlp_status"),
    getRunNote: (runId: string) =>
      getJson<RunNote | null>(`/__tare/run_note?run_id=${encodeURIComponent(runId)}`),
    saveRunNote: (note: RunNote) => postJson<{ ok: boolean }>("/__tare/notes", note).then(() => undefined),
    deleteRunNote: (runId: string) =>
      postJson<{ ok: boolean }>("/__tare/notes/delete", { run_id: runId }).then(() => undefined),
    runsByTag: (tag: string) => getJson<string[]>(`/__tare/notes_by_tag?tag=${encodeURIComponent(tag)}`),
    starredRuns: () => getJson<string[]>("/__tare/starred_runs"),
    acknowledgeAnomaly: (key: string) =>
      postJson<{ ok: boolean }>("/__tare/acknowledge", { key }).then(() => undefined),
    seedDemo: () => postJson<{ ok: boolean }>("/__tare/demo", {}).then(() => "demo"),
    purgeTranscripts: () =>
      postJson<{ ok: boolean }>("/__tare/transcript_purge", {}).then(() => undefined),
    configOrigins: () => Promise.resolve({}),
    // Calibrated Bench cohort analysis: POST the typed request, receive the
    // AnalysisResponse{data,provenance} envelope. The server wraps the shared cohort_*_json fns.
    resolveCohort: (spec: CohortSpec) =>
      postJson<AnalysisResponse<CohortResolveResult>>("/__tare/cohort/resolve", spec),
    facetCohort: (req: CohortFacetRequest) =>
      postJson<AnalysisResponse<CohortFacetResult>>("/__tare/cohort/facets", req),
    compareCohort: (req: CohortCompareRequest) =>
      postJson<AnalysisResponse<CohortCompareResult>>("/__tare/cohort/compare", req),
    searchCohort: (req: CohortSearchRequest) =>
      postJson<AnalysisResponse<CohortSearchResult>>("/__tare/cohort/search", req),
    timelineCohort: (req: CohortTimelineRequest) =>
      postJson<AnalysisResponse<CohortTimelineResult>>("/__tare/cohort/timeline", req),
    anomalyWhy: (req: AnomalyWhyRequest) => postJson<AnomalyWhy[]>("/__tare/anomaly_why", req),
    runExperiment: (req: ExperimentRequest) =>
      postJson<ExperimentResult>("/__tare/experiment", req),
    listInvestigations: () =>
      getJson<SavedInvestigationV2[]>("/__tare/investigations").then((rows) =>
        rows.map(upgradeInvestigationEntityRefs)
      ),
    saveInvestigation: (inv: SavedInvestigationV2) =>
      postJson<{ ok: boolean }>("/__tare/investigations", inv).then(() => undefined),
    deleteInvestigation: (id: string) =>
      postJson<{ ok: boolean }>("/__tare/investigations/delete", { id }).then(() => undefined),
    acceptSavings: (req: SavingsActionRequest) =>
      postJson<{ ok: boolean }>("/__tare/savings/accept", req).then(() => undefined),
    dismissSavings: (req: SavingsActionRequest) =>
      postJson<{ ok: boolean }>("/__tare/savings/dismiss", req).then(() => undefined),
    unacceptSavings: (id: SavingsActionIdentity) =>
      postJson<{ ok: boolean }>("/__tare/savings/unaccept", id).then(() => undefined),
    savingsActions: () => getJson<SavingsAction[]>("/__tare/savings/actions"),
    verifySavings: (req: SavingsVerifyRequest) =>
      postJson<SavingsVerifyResult>("/__tare/savings/verify", req),
  };
}

function trendQs(q: TrendQuery): string {
  const p = new URLSearchParams();
  if (q.by) p.set("by", q.by);
  if (q.from) p.set("from", q.from);
  if (q.to) p.set("to", q.to);
  if (q.window != null) p.set("window", String(q.window));
  if (q.threshold != null) p.set("threshold", String(q.threshold));
  return p.toString();
}
