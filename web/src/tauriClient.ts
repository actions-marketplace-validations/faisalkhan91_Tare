// Tauri-backed TareClient: the desktop window talks to the Rust core via `invoke(...)` instead
// of the loopback HTTP read API. The shared shell (main.ts) is identical across both — only the
// client differs. Commands that return a JSON string are parsed; object commands pass through.
// `explain` returns a raw narrative string (not JSON).

import type {
  Anomaly,
  AnomalyWhy,
  AnomalyWhyRequest,
  CacheAdvice,
  FlameDiffModel,
  CostRegression,
  PricingInfo,
  ProxyStatus,
  ReceiptResult,
  OtlpStatus,
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
import type { FlamegraphModel } from "./svg.js";
import type { TrendReport } from "./trendSvg.js";
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

type Invoke = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

function defaultInvoke(): Invoke {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const inv = (globalThis as any).__TAURI__?.core?.invoke;
  if (!inv) throw new Error("Tauri invoke bridge unavailable");
  return inv as Invoke;
}

export function createTauriClient(invoke: Invoke = defaultInvoke()): TareClient {
  // A command whose Rust return is a JSON string -> parse to T.
  const j = async <T>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
    JSON.parse((await invoke(cmd, args)) as string) as T;

  return {
    listRuns: () => invoke("list_runs") as Promise<string[]>,
    flamegraph: (runId) => invoke("run_flamegraph", { runId }) as Promise<FlamegraphModel>,
    profile: (runId, sort, topN) =>
      invoke("run_profile", { runId, sort, topN }) as Promise<ProfileTable>,
    frontier: () => invoke("cost_frontier", {}) as Promise<Frontier>,
    saveQuality: (runId: string, score: number, source = "cli") =>
      invoke("set_run_quality", { runId, score, source }) as Promise<void>,
    report: () => j<Report>("report"),
    trend: (q: TrendQuery) => j<TrendReport>("trend", { by: q.by, from: q.from, to: q.to }),
    anomalies: (q?: TrendQuery) =>
      j<Anomaly[]>("anomalies", { by: q?.by, window: q?.window, threshold: q?.threshold }),
    costRegressions: (q?: { window?: number; threshold?: number }) =>
      j<CostRegression[]>("cost_regressions", { window: q?.window, threshold: q?.threshold }),
    today: async () => {
      const t = (await invoke("today_spend")) as TodaySpend;
      return {
        run_count: t.run_count ?? 0,
        total_micros: t.total_micros,
        pricing_version: t.pricing_version ?? "",
        effective_date: t.effective_date ?? "",
      };
    },
    runStatus: (runId) => j<RunStatus>("run_status", { runId }),
    runStatuses: () => j<RunStatus[]>("run_statuses", {}),
    runMeta: (runId) => j<RunMeta>("run_meta", { runId }),
    runSteps: (runId) => j<RunStep[]>("run_steps", { runId }),
    transcript: (runId, step) =>
      j<TranscriptStep | null>("transcript", { runId, step }),
    recentSteps: (n?: number) => j<RecentStep[]>("recent_steps", { n }),
    explain: (runId) => invoke("explain", { runId }) as Promise<string>,
    sessionAutopsy: (runId, median) => j<SessionAutopsy>("session_autopsy", { runId, median }),
    advise: () => j<CacheAdvice[]>("advise"),
    savings: () => j<SavingsLedger>("savings"),
    actionPlan: () => j<ActionPlan>("action_plan"),
    cacheLedger: () => j<CacheLedger>("cache_ledger"),
    reasoning: () => j<ReasoningBreakout>("reasoning"),
    effectiveness: () => j<Effectiveness>("effectiveness"),
    confidence: () => j<EstimateConfidence>("confidence"),
    whatif: (crossProvider) => j<WhatIfRecommendations>("whatif", { crossProvider }),
    diff: (a, b) => j<ReportDiff>("diff", { a, b }),
    flameDiff: (a, b, normalized = false) =>
      j<FlameDiffModel>("flame_diff", { a, b, normalized }),
    pricing: () => j<PricingInfo>("pricing"),
    receipt: (runId, maxPrivate = false) => j<ReceiptResult>("receipt", { runId, maxPrivate }),
    rollup: (by: string, filter?: { by: string; label: string }) =>
      j<RollupReport>("rollup", { by, filterBy: filter?.by, filter: filter?.label }),
    punchcard: () => j<PunchcardModel>("punchcard"),
    heatmap: () => j<HeatmapModel>("heatmap"),
    sessions: () => j<SessionReport>("sessions"),
    correlate: () => j<CorrelationReport>("correlate"),
    lineages: () => j<LineageReport[]>("lineages"),
    units: () => j<UnitReport>("units"),
    sessionsLive: () => j<SessionLive[]>("sessions_live"),
    vendorToday: () => j<VendorToday>("vendor_today"),
    burnrate: (range?: BurnRateRange) => j<BurnRate>("burnrate", { range }),
    coverage: () => j<Coverage>("coverage"),
    reconcile: (day?: string) => j<Reconciliation>("reconcile", { day }),
    loops: () => j<LoopWasteReport>("loops"),
    failures: () => j<FailureWasteReport>("failures"),
    lenses: () => j<Lenses>("lenses"),
    budget: () => j<PeriodBudget>("budget"),
    sandwich: (component: string) => j<Sandwich>("sandwich", { component }),
    otlpStatus: () => j<OtlpStatus>("otlp_status"),
    exportRun: (runId: string, format: string) =>
      invoke("export", { runId, format }) as Promise<string>,
    config: () => j<TareConfigDto>("get_config"),
    saveConfig: async (config: Partial<TareConfigDto>) => {
      await invoke("save_config", { configJson: JSON.stringify(config) });
    },
    canSaveConfig: () => true,
    canControlProxy: () => true,
    proxyStatus: () => j<ProxyStatus>("proxy_status"),
    proxyStart: (port?: number) => j<ProxyStatus>("start_proxy", { port }),
    proxyStop: () => j<ProxyStatus>("stop_proxy"),
    canNotify: () => true,
    notify: async (title: string, body: string) => {
      await invoke("notify", { title, body });
    },
    // Window-close-to-background preference: the desktop reads/writes background.json
    // via the Rust commands; the close policy consumes it on the next close.
    canBackgroundOnClose: () => true,
    getBackgroundOnClose: () => invoke("get_background_on_close") as Promise<boolean>,
    setBackgroundOnClose: async (enabled: boolean) => {
      await invoke("set_background_on_close", { enabled });
    },
    // Run notes: invoke the desktop commands (adapter fns in tare-tauri).
    getRunNote: (runId: string) => j<RunNote | null>("run_note", { runId }),
    saveRunNote: async (note: RunNote) => {
      await invoke("save_run_note", { noteJson: JSON.stringify(note) });
    },
    deleteRunNote: async (runId: string) => {
      await invoke("delete_run_note", { runId });
    },
    runsByTag: (tag: string) => j<string[]>("runs_by_tag", { tag }),
    starredRuns: () => j<string[]>("starred_runs"),
    acknowledgeAnomaly: async (key: string) => {
      await invoke("acknowledge_anomaly", { key });
    },
    seedDemo: () => invoke("seed_demo").then((r) => (typeof r === "string" ? r : "demo")),
    purgeTranscripts: async () => {
      await invoke("transcript_purge");
    },
    configOrigins: () => j<Record<string, string>>("config_origins"),
    // Calibrated Bench cohort analysis: the commands delegate to the SAME shared
    // tare-cli cohort_*_json fns the HTTP routes call, so desktop and browser return equivalent
    // data. Each command returns the AnalysisResponse JSON string, parsed by `j`. The typed request
    // is passed as a JSON `body` string (the command re-parses it into the Rust DTO).
    resolveCohort: (spec: CohortSpec) =>
      j<AnalysisResponse<CohortResolveResult>>("cohort_resolve", { body: JSON.stringify(spec) }),
    facetCohort: (req: CohortFacetRequest) =>
      j<AnalysisResponse<CohortFacetResult>>("cohort_facets", { body: JSON.stringify(req) }),
    compareCohort: (req: CohortCompareRequest) =>
      j<AnalysisResponse<CohortCompareResult>>("cohort_compare", { body: JSON.stringify(req) }),
    searchCohort: (req: CohortSearchRequest) =>
      j<AnalysisResponse<CohortSearchResult>>("cohort_search", { body: JSON.stringify(req) }),
    timelineCohort: (req: CohortTimelineRequest) =>
      j<AnalysisResponse<CohortTimelineResult>>("cohort_timeline", { body: JSON.stringify(req) }),
    anomalyWhy: (req: AnomalyWhyRequest) =>
      j<AnomalyWhy[]>("anomaly_why", { body: JSON.stringify(req) }),
    runExperiment: (req: ExperimentRequest) =>
      j<ExperimentResult>("experiment", { body: JSON.stringify(req) }),
    listInvestigations: () =>
      j<SavedInvestigationV2[]>("list_investigations").then((rows) =>
        rows.map(upgradeInvestigationEntityRefs)
      ),
    saveInvestigation: async (inv: SavedInvestigationV2) => {
      await invoke("save_investigation", { body: JSON.stringify(inv) });
    },
    deleteInvestigation: async (id: string) => {
      await invoke("delete_investigation", { id });
    },
    acceptSavings: async (req: SavingsActionRequest) => {
      await invoke("savings_accept", { body: JSON.stringify(req) });
    },
    dismissSavings: async (req: SavingsActionRequest) => {
      await invoke("savings_dismiss", { body: JSON.stringify(req) });
    },
    unacceptSavings: async (id: SavingsActionIdentity) => {
      await invoke("savings_unaccept", { body: JSON.stringify(id) });
    },
    savingsActions: () => j<SavingsAction[]>("savings_actions"),
    verifySavings: (req: SavingsVerifyRequest) =>
      j<SavingsVerifyResult>("savings_verify", { body: JSON.stringify(req) }),
  };
}
