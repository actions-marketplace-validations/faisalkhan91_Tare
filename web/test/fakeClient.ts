// Shared test double: a fully-implemented TareClient with neutral defaults, overridable
// per-test. Centralizes the client surface so new methods land in one place.

import type { RunNote, TareClient } from "../src/client.js";
import type { TrendReport } from "../src/trendSvg.js";
import type {
  AnalysisProvenance,
  CohortSpec,
  SavingsAction,
  SavingsActionRequest,
} from "../src/analysis/types.js";
import type { SavedInvestigationV2 } from "../src/analysis/investigation.js";
import { cohortHash } from "../src/analysis/serialize.js";

/// Persist a savings action into the fake's in-memory map, keyed like the store on
/// (opportunity_key, cohort_hash). Records the computed compatibility warning for aggregate-only.
function putSavingsAction(
  rows: Map<string, SavingsAction>,
  req: SavingsActionRequest,
  status: string
): void {
  const hash = cohortHash(req.cohort);
  rows.set(`${req.opportunity_key} ${hash}`, {
    opportunity_key: req.opportunity_key,
    cohort_hash: hash,
    status,
    acted_at: "1970-01-01T00:00:00Z",
    cohort: req.cohort,
    baseline: req.baseline,
    match: req.match,
    metric: req.metric,
    normalization: req.normalization,
    outcome_denominator: req.outcome_denominator,
    expected_low_micros: req.expected_low_micros,
    expected_point_micros: req.expected_point_micros,
    expected_high_micros: req.expected_high_micros,
    quality_guardrail: req.quality_guardrail,
    compatibility_warnings:
      req.match.kind === "aggregate_only"
        ? [
            "aggregate-only action — observed association, not a causal saving; before/after units are not matched",
          ]
        : [],
  });
}

/// Neutral, honest provenance for the fake (mirrors the CLI's out-of-band floor): coverage unknown,
/// coarse fidelity, derived-from-provider-counts. Echoes the scope like the real endpoint.
export function fakeProvenance(scope: CohortSpec): AnalysisProvenance {
  return {
    refreshed_at: "1970-01-01T00:00:00Z",
    scope,
    capture_sources: [],
    coverage_status: "unknown",
    component_fidelity: "coarse",
    pricing_edition: { version: "fake", effective_date: "1970-01-01", mode: "effective" },
    allocation_method: "provider_counts",
    value_class: "derived",
    assumptions: [],
  };
}

export const EMPTY_TREND: TrendReport = {
  dimension: "total",
  from: "",
  to: "",
  days: [],
  series: [],
  pricing_version: "x",
  estimated: true,
};

/**
 * Class-friendly counterpart to {@link fakeClient} for legacy fixtures that use
 * inheritance. Defaults are installed only when neither the class nor one of its
 * subclasses implements a client method, so prototype overrides keep working.
 *
 * The merged interface makes the runtime-completed class expose the full client
 * contract to TypeScript without duplicating the large production interface here.
 */
export interface TestTareClient extends TareClient {}
export class TestTareClient {
  constructor(overrides: Partial<TareClient> = {}) {
    for (const [key, value] of Object.entries(fakeClient(overrides))) {
      if (key in this) continue;
      Object.defineProperty(this, key, {
        configurable: true,
        enumerable: true,
        value,
        writable: true,
      });
    }
  }
}

export function fakeClient(overrides: Partial<TareClient> = {}): TareClient {
  const notes = new Map<string, RunNote>(); // in-memory run notes
  const investigations = new Map<string, SavedInvestigationV2>(); // in-memory saved investigations
  const savingsActionRows = new Map<string, SavingsAction>(); // In-memory savings actions.
  return {
    listRuns: async () => [],
    flamegraph: async () => {
      throw new Error("unused");
    },
    profile: async (runId: string) => ({
      run_id: runId,
      pricing_version: "x",
      sort: "cum" as const,
      total_micros: 0,
      total_tokens: 0,
      rows: [],
    }),
    frontier: async () => ({
      points: [],
      has_quality: false,
      pricing_version: "x",
      estimated: true,
    }),
    saveQuality: async () => undefined,
    report: async () => ({
      pricing_version: "x",
      effective_date: "2026-06-01",
      estimated: true,
      total_micros: 0,
      rows: [],
    }),
    trend: async () => EMPTY_TREND,
    anomalies: async () => [],
    costRegressions: async () => [],
    seedDemo: async () => "demo",
    today: async () => ({ run_count: 0, total_micros: 0, pricing_version: "x", effective_date: "2026-06-01" }),
    runStatus: async (run_id: string) => ({ run_id, micros: 0, steps: 0, top_cause: null }),
    runStatuses: async () => [],
    runMeta: async (run_id: string) => ({
      run_id,
      created_date: "2026-06-29",
      privacy_policy_id: null,
      profile: null,
      steps: 0,
      models: [],
      providers: [],
      sources: [],
      stop_reasons: [],
      pricing_version: "x",
      effective_date: "2026-06-01",
    }),
    runSteps: async () => [],
    transcript: async () => null,
    purgeTranscripts: async () => {},
    recentSteps: async () => [],
    explain: async () => "",
    sessionAutopsy: async (runId: string) => ({
      run_id: runId,
      total_micros: 0,
      classes: [
        { class: "fresh_input", micros: 0 },
        { class: "cache_read", micros: 0 },
        { class: "cache_write", micros: 0 },
        { class: "output", micros: 0 },
      ],
      reasoning_micros: 0,
      cache_hit_pct: 0,
      vs_median_pct: null,
      fidelity: "cost_class",
      headline: { kind: "efficient" as const },
      opportunities: [],
    }),
    advise: async () => [],
    savings: async () => ({
      opportunities: [],
      total_recoverable_micros: 0,
      total_spend_micros: 0,
      savings_index: 100,
      pricing_version: "x",
      estimated: true,
    }),
    actionPlan: async () => ({
      items: [],
      recoverable_floor_micros: 0,
      at_risk_total_micros: 0,
      total_spend_micros: 0,
      savings_index: 100,
      pricing_version: "x",
      estimated: true,
    }),
    cacheLedger: async () => ({
      cache_read_tokens: 0,
      saved_micros: 0,
      read_cost_micros: 0,
      by_model: [],
    }),
    reasoning: async () => ({
      reasoning_tokens: 0,
      answer_tokens: 0,
      reasoning_micros: 0,
      output_micros: 0,
      reasoning_pct: 0,
      by_model: [],
    }),
    effectiveness: async () => ({
      cost_micros: 0,
      per_pull_request_micros: null,
      per_commit_micros: null,
      per_1k_loc_micros: null,
      per_active_hour_micros: null,
      per_session_micros: null,
      accept_rate_pct: null,
      cost_per_successful_run_micros: null,
      run_success_rate_pct: null,
      estimated: true,
    }),
    confidence: async () => ({
      pricing_age_days: 0,
      unpriced_token_share_pct: 0,
      coverage_status: "full",
      coverage_share_pct: 100,
      label: "high",
      estimated: true,
    }),
    whatif: async () => ({
      baseline_micros: 0,
      recommendations: [],
      estimated: true,
      approximate: true,
    }),
    diff: async () => ({
      pricing_version: "x",
      estimated: true,
      total_before: 0,
      total_after: 0,
      delta_micros: 0,
      rows: [],
    }),
    flameDiff: async (a: string, b: string, normalized = false) => ({
      run_a: a,
      run_b: b,
      normalized,
      total_a_micros: 0,
      total_b_micros: 0,
      root: {
        name: "root",
        tokens_a: 0,
        tokens_b: 0,
        micros_a: 0,
        micros_b: 0,
        delta_micros: 0,
        share_a_bps: 0,
        share_b_bps: 0,
        delta_bps: 0,
        children: [],
      },
    }),
    pricing: async () => ({ version: "x", effective_date: "2026-06-01", note: null }),
    receipt: async () => ({
      receipt: {},
      verify: {
        scope: "run:x",
        pricing_version: "x",
        recomputed_total_micros: 0,
        rows: 0,
        flamegraph_checked: false,
        digest: 0,
      },
    }),
    rollup: async (_by: string, _filter?: { by: string; label: string }) => ({
      dimension: _by,
      rows: [],
      total_micros: 0,
      pricing_version: "x",
      estimated: true,
    }),
    punchcard: async () => ({ cells: [], max_micros: 0, total_micros: 0 }),
    heatmap: async () => ({ cells: [], max_micros: 0, total_micros: 0, weeks: 0 }),
    sessionsLive: async () => [],
    vendorToday: async () => ({ cost_micros: 0, tokens: 0, day: "2026-06-29", available: false }),
    burnrate: async () => ({
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
      period_start: "2026-09-01",
      as_of: "2026-09-01",
      period_end: "2026-09-30",
    }),
    coverage: async () => ({ status: "none" as const, has_proxy: false, has_otel: false, blind_sources: [], sources: [] }),
    reconcile: async () => ({
      day: "2026-06-29",
      pricing_version: "x",
      rows: [],
      estimate_total_micros: 0,
      vendor_total_micros: 0,
      delta_total_micros: 0,
      has_vendor: false,
    }),
    sessions: async () => ({
      rows: [],
      total_micros: 0,
      pricing_version: "x",
      estimated: true,
    }),
    correlate: async () => ({
      rows: [],
      pricing_version: "x",
      estimated: true,
    }),
    lineages: async () => [],
    units: async () => ({
      rows: [],
      unbucketed_runs: 0,
      total_micros: 0,
      pricing_version: "x",
      estimated: true,
    }),
    loops: async () => ({
      rows: [],
      total_micros: 0,
      total_redundant_steps: 0,
      pricing_version: "x",
      estimated: true,
    }),
    failures: async () => ({
      rows: [],
      total_micros: 0,
      total_failed_steps: 0,
      pct_of_spend: 0,
      pricing_version: "x",
      estimated: true,
    }),
    sandwich: async () => ({
      component: "system",
      total_micros: 0,
      runs: [],
      pricing_version: "x",
      estimated: true,
    }),
    budget: async () => ({
      period: "month",
      spent_micros: 0,
      cap_micros: 0,
      warn_pct: 80,
      pct: 0,
      status: "ok",
    }),
    lenses: async () => ({
      input_micros: 0,
      output_micros: 0,
      cache_saved_micros: 0,
      total_micros: 0,
      calls: 0,
      micros_per_call: 0,
      total_tokens: 0,
      output_tokens: 0,
      input_tokens: 0,
      cache_read_tokens: 0,
      pricing_version: "x",
      estimated: true,
    }),
    exportRun: async () => "{}",
    config: async () => ({ budget: {}, privacy: {}, providers: {}, proxy: {} }),
    saveConfig: async () => {},
    canSaveConfig: () => true,
    canControlProxy: () => false,
    proxyStatus: async () => ({ running: false, port: 0, url: "" }),
    proxyStart: async () => ({ running: true, port: 8788, url: "http://127.0.0.1:8788" }),
    proxyStop: async () => ({ running: false, port: 0, url: "" }),
    canNotify: () => false,
    notify: async () => {},
    canBackgroundOnClose: () => false,
    getBackgroundOnClose: async () => false,
    setBackgroundOnClose: async () => {},
    otlpStatus: async () => ({
      listening: false,
      port: 4318,
      events: 0,
      last_event_unix: 0,
      age_seconds: null,
    }),
    getRunNote: async (runId: string) => notes.get(runId) ?? null,
    saveRunNote: async (note) => {
      notes.set(note.run_id, { ...note });
    },
    deleteRunNote: async (runId: string) => {
      notes.delete(runId);
    },
    runsByTag: async (tag: string) =>
      [...notes.values()].filter((n) => n.tags.includes(tag)).map((n) => n.run_id),
    starredRuns: async () => [...notes.values()].filter((n) => n.starred).map((n) => n.run_id),
    acknowledgeAnomaly: async () => {},
    configOrigins: async () => ({}),
    // Calibrated Bench cohort analysis: empty-but-honest envelopes so headless UI
    // tests can exercise the Investigate/Compare screens; per-test overrides supply real data.
    resolveCohort: async (spec) => ({
      data: {
        run_ids: [],
        run_count: 0,
        step_count: 0,
        total_micros: 0,
        entity_rows: [],
      },
      provenance: fakeProvenance(spec),
    }),
    facetCohort: async (req) => ({
      data: { dimension: req.dimension, rows: [] },
      provenance: fakeProvenance(req.selection),
    }),
    compareCohort: async (req) => ({
      data: {
        selection: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
        baseline: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
        total_delta_micros: 0,
        volume_delta_micros: 0,
        size_delta_micros: 0,
        efficiency_delta_micros: 0,
        compatibility_warnings: [],
      },
      provenance: fakeProvenance(req.selection),
    }),
    searchCohort: async (req) => ({
      data: { entities: [], truncated: false },
      provenance: fakeProvenance(req.cohort),
    }),
    timelineCohort: async (req) => ({
      data: {
        days: req.cohort.from && req.cohort.to ? [req.cohort.from] : [],
        run_count: 0,
        unit: req.cohort.metric === "tokens" ? "tokens" : req.cohort.metric === "cache_hit_rate" ? "percent" : "estimated_micro_usd",
        series: [],
        config_events: [],
      },
      provenance: fakeProvenance(req.cohort),
    }),
    anomalyWhy: async () => [],
    runExperiment: async () => ({
      cells: [],
      pareto: [],
      baseline_micros: 0,
      best_micros: 0,
      best_saving_micros: 0,
      pricing_version: "x",
      estimated: true,
      approximate: false,
    }),
    // In-memory saved-investigations store (mirrors the SQLite source of truth) so headless UI
    // tests can round-trip. Newest-updated first, keyed on id.
    listInvestigations: async () =>
      [...investigations.values()].sort((a, b) => b.updated_at.localeCompare(a.updated_at)),
    saveInvestigation: async (inv) => {
      investigations.set(inv.id, inv);
    },
    deleteInvestigation: async (id) => {
      investigations.delete(id);
    },
    // In-memory savings-action lifecycle (mirrors the SQLite table) so headless UI tests can
    // exercise apply/dismiss/unaccept. Keyed on (opportunity_key, cohort_hash), like the store.
    acceptSavings: async (req) => {
      putSavingsAction(savingsActionRows, req, "applied");
    },
    dismissSavings: async (req) => {
      putSavingsAction(savingsActionRows, req, "dismissed");
    },
    unacceptSavings: async (id) => {
      savingsActionRows.delete(`${id.opportunity_key} ${id.cohort_hash}`);
    },
    savingsActions: async () => [...savingsActionRows.values()],
    verifySavings: async () => ({
      status: "verifying" as const,
      complete: false,
      selection_before_micros: 0,
      selection_after_micros: 0,
      observed_reduction_micros: 0,
      matched_before: 0,
      matched_after: 0,
      unmatched_before: 0,
      unmatched_after: 0,
      compatibility_warnings: [],
    }),
    ...overrides,
  };
}
