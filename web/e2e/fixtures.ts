// Deterministic API fixtures for the E2E shell and product journeys. The shell boots against
// the loopback HTTP read API; here we intercept every `/__tare/*` call and return a benign, minimal
// payload so screens render their (possibly empty) state without a live backend and without hanging
// on a refused connection. Mutable workflows opt into a per-test action/capture controller so their
// mutations are real within the journey but never touch a database or network. The cohort fixture is
// intentionally rich enough to prove Investigate/Run Profile and bounded 1,000-row rendering.

import type { Page, Route } from "@playwright/test";
import { cohortHash } from "../src/analysis/serialize.js";
import type { CohortSpec } from "../src/analysis/types.js";

const COHORT: CohortSpec = {
  from: null,
  to: null,
  timezone: "UTC",
  entity: "run",
  filters: [],
  pricing: { mode: "effective_dated" },
  metric: "spend_micros",
  normalization: "absolute",
  outcome_denominator: null,
};
const PROVENANCE = {
  refreshed_at: "2026-07-15T00:00:00Z",
  scope: COHORT,
  capture_sources: ["proxy", "otel"],
  coverage_status: "full",
  priced_token_share_pct: 98,
  component_fidelity: "component",
  pricing_edition: { version: "fixture", effective_date: "2026-07-01", mode: "effective" },
  allocation_method: "captured_component",
  value_class: "observed",
  assumptions: ["Deterministic local fixture."],
};

const WORKFLOW_COHORT: CohortSpec = {
  ...COHORT,
  from: "2026-07-01",
  to: "2026-07-07",
  timezone: "America/Los_Angeles",
  filters: [{ op: "eq", dimension: "workload_key", value: "nightly" }],
};
const WORKFLOW_BASELINE_COHORT: CohortSpec = {
  ...WORKFLOW_COHORT,
  from: "2026-06-24",
  to: "2026-06-30",
};
const WORKFLOW_OPPORTUNITY = {
  opportunity_key: "rightsizing:nightly-classifier",
  kind: "rightsizing",
  label: "Right-size the nightly classifier",
  recoverable_micros: 3_000_000,
  confidence: "projected",
  fix_text: 'model = "claude-haiku-4-5"',
  effort: "S",
  affected_run_count: 4,
  affected_step_count: 12,
  affected_run_ids: ["run-profile-fixture"],
  affected_steps: [{ run_id: "run-profile-fixture", step_ordinal: 1 }],
  evidence_truncated: false,
  evidence_method: "deterministic rightsizing detector",
  cohort_snapshot: WORKFLOW_COHORT,
  assumptions: ["Captured token counts remain representative during the verification window."],
  quality_risk: "Confirm the stored quality threshold before a broad rollout.",
};
const WORKFLOW_SAVINGS = {
  opportunities: [
    {
      kind: WORKFLOW_OPPORTUNITY.kind,
      label: WORKFLOW_OPPORTUNITY.label,
      recoverable_micros: WORKFLOW_OPPORTUNITY.recoverable_micros,
      confidence: WORKFLOW_OPPORTUNITY.confidence,
      fix_text: WORKFLOW_OPPORTUNITY.fix_text,
      effort: WORKFLOW_OPPORTUNITY.effort,
    },
  ],
  opportunities_v2: [WORKFLOW_OPPORTUNITY],
  total_recoverable_micros: 3_000_000,
  capped_potential_micros: 3_000_000,
  applied_micros: 0,
  observed_micros: 0,
  total_spend_micros: 10_000_000,
  savings_index: 70,
  pricing_version: "2026.07.10",
  estimated: true,
};
const WORKFLOW_INVESTIGATION = {
  id: "matched-nightly",
  label: "Matched nightly cohort",
  created_at: "2026-07-16T12:00:00Z",
  updated_at: "2026-07-16T12:00:00Z",
  state: {
    workspace: "optimize",
    scope: WORKFLOW_COHORT,
    selection: WORKFLOW_COHORT,
    baseline: {
      kind: "prior_window",
      label: "Prior window · 4 runs",
      cohort: WORKFLOW_BASELINE_COHORT,
      sampleCount: 4,
    },
    match: { kind: "workload_key", key: "nightly" },
    comparison: [],
    pinned: null,
  },
};
const WORKFLOW_EXPERIMENT = {
  cells: [
    {
      coords: [{ axis: "model", value: "claude-haiku-4-5" }],
      label: ["model=claude-haiku-4-5"],
      cost_micros: 4_000_000,
      approximate: true,
    },
    {
      coords: [{ axis: "model", value: null }],
      label: ["model=as-captured"],
      cost_micros: 10_000_000,
      approximate: false,
    },
  ],
  pareto: [0],
  baseline_micros: 10_000_000,
  best_micros: 4_000_000,
  best_saving_micros: 6_000_000,
  pricing_version: "2026.07.10",
  estimated: true,
  approximate: true,
};

// Enough opaque IDs to exercise the bounded/virtual run navigator and independent list scrolling.
// Keep the selected fixture first; the remaining IDs are intentionally content-free.
const RUN_IDS = [
  "run-profile-fixture",
  ...Array.from({ length: 159 }, (_, index) => `captured-run-${String(index + 1).padStart(3, "0")}`),
];
const COHORT_ENTITIES = [
  {
    entity: { run_id: "run-profile-fixture" },
    matched_micros: 2_400_000,
    whole_entity_micros: 2_400_000,
    matched_step_count: 2,
  },
  ...Array.from({ length: 999 }, (_, index) => {
    const micros = 900_000 - index * 500;
    return {
      entity: { run_id: `cohort-run-${String(index + 1).padStart(4, "0")}` },
      matched_micros: micros,
      whole_entity_micros: micros,
      matched_step_count: 2,
    };
  }),
];
const COHORT_TOTAL_MICROS = COHORT_ENTITIES.reduce((sum, row) => sum + row.matched_micros, 0);

// Inclusive calendar-day range (UTC) — deterministic given fixed from/to strings.
function calendarRange(from: string, to: string): string[] {
  const days: string[] = [];
  const end = new Date(`${to}T00:00:00Z`);
  for (let d = new Date(`${from}T00:00:00Z`); d <= end; d.setUTCDate(d.getUTCDate() + 1)) {
    days.push(d.toISOString().slice(0, 10));
  }
  return days;
}

function runIdsFromFilters(filters: unknown): string[] {
  return (Array.isArray(filters) ? filters : []).flatMap((f) =>
    f && typeof f === "object" && (f as { op?: string }).op === "run_ids"
      ? ((f as { ids?: string[] }).ids ?? [])
      : []
  );
}

// Timeline and Compare use request-aware responses. These endpoints are absent
// from the static BY_PATH table because they must ECHO the request (the navigated run pair / brushed
// window) to be a faithful, deterministic journey backend. Returns undefined for anything else so the
// static table still handles it. Kept honest: estimated language, explicit compatibility warnings,
// user-supplied quality never invented (quality lives in the frontier fixture, not here).
function analysisBody(pathname: string, method: string, request: Record<string, unknown>, url: URL): unknown {
  const last = pathname.split("/").filter(Boolean).pop() ?? "";
  if (last === "timeline" && method === "POST") {
    const cohort = (request.cohort ?? {}) as { from?: string; to?: string };
    const range = cohort.from && cohort.to ? calendarRange(cohort.from, cohort.to) : ["2026-07-01"];
    const runCount = 5;
    return {
      data: {
        days: range,
        run_count: runCount,
        unit: "estimated_micro_usd",
        series: [
          {
            key: "total",
            points: range.map((day, index) => ({ day, value: (index + 1) * 1_000_000, support_count: runCount })),
          },
        ],
        // A deterministic config-change marker on the 4th day when present (annotation coverage).
        config_events: range.includes("2026-07-04")
          ? [{ occurred_at: "2026-07-04T12:00:00Z", day: "2026-07-04", source: "settings", changed_fields: ["capture.mode"] }]
          : [],
      },
      provenance: PROVENANCE,
    };
  }
  if (last === "compare" && method === "POST") {
    const selection = (request.selection ?? {}) as { filters?: unknown };
    const baseline = (request.baseline ?? {}) as { filters?: unknown };
    const sel = runIdsFromFilters(selection.filters);
    const base = runIdsFromFilters(baseline.filters);
    return {
      data: {
        selection: { run_ids: sel, run_count: sel.length, step_count: sel.length * 2, total_micros: 15_000_000, entity_rows: [] },
        baseline: { run_ids: base, run_count: base.length, step_count: base.length * 2, total_micros: 10_000_000, entity_rows: [] },
        total_delta_micros: 5_000_000,
        total_delta_pct: 50,
        // Components sum exactly to total_delta; efficiency countervails.
        volume_delta_micros: 1_000_000,
        size_delta_micros: 6_000_000,
        efficiency_delta_micros: -2_000_000,
        compatibility_warnings: ["Workloads differ: matched on 2 of 3 unit keys; unmatched units are excluded and reported separately."],
      },
      provenance: PROVENANCE,
    };
  }
  if (last === "flame_diff" && method === "GET") {
    const a = url.searchParams.get("a") ?? "run-a";
    const b = url.searchParams.get("b") ?? "run-b";
    const normalized = url.searchParams.get("normalized") === "true" || url.searchParams.get("normalized") === "1";
    return {
      run_a: a,
      run_b: b,
      normalized,
      total_a_micros: 10_000_000,
      total_b_micros: 15_000_000,
      root: {
        name: "root", tokens_a: 0, tokens_b: 0, micros_a: 10_000_000, micros_b: 15_000_000,
        delta_micros: 5_000_000, share_a_bps: 10_000, share_b_bps: 10_000, delta_bps: 0,
        children: [
          { name: "tools", tokens_a: 0, tokens_b: 0, micros_a: 4_000_000, micros_b: 9_000_000, delta_micros: 5_000_000, share_a_bps: 4_000, share_b_bps: 6_000, delta_bps: 2_000, children: [] },
          { name: "system", tokens_a: 0, tokens_b: 0, micros_a: 6_000_000, micros_b: 6_000_000, delta_micros: 0, share_a_bps: 6_000, share_b_bps: 4_000, delta_bps: -2_000, children: [] },
        ],
      },
    };
  }
  if (last === "diff" && method === "GET") {
    return {
      pricing_version: "fixture",
      estimated: true,
      total_before: 10_000_000,
      total_after: 15_000_000,
      delta_micros: 5_000_000,
      rows: [
        { cause: "bigger-context", micros_before: 2_000_000, micros_after: 9_000_000, delta_micros: 7_000_000 },
        { cause: "cache-reuse", micros_before: 3_000_000, micros_after: 1_000_000, delta_micros: -2_000_000 },
      ],
    };
  }
  return undefined;
}

// Minimal well-formed responses keyed by the trailing path segment. Anything unmatched falls back to
// an empty array (list endpoints) so a fetch never rejects.
const BY_PATH: Record<string, unknown> = {
  today: { total_micros: 0, pricing_version: "fixture", effective_date: "2026-07-01" },
  config: { budget: {}, privacy: {}, providers: {}, proxy: {} },
  runs: RUN_IDS,
  report: { total_micros: 0, causes: [], unpriced: [], estimated: true },
  trend: {
    dimension: "total",
    from: "2026-07-15",
    to: "2026-07-15",
    days: [],
    series: [],
    pricing_version: "fixture",
    estimated: true,
  },
  burnrate: {
    run_rate_micros_per_day: 2_000_000,
    effective_rate_micros_per_day: 2_000_000,
    spent_micros: 28_000_000,
    active_days: 14,
    daily_spend_micros: [
      1_200_000, 1_400_000, 1_500_000, 1_600_000, 1_700_000, 1_800_000, 1_900_000,
      2_000_000, 2_100_000, 2_200_000, 2_300_000, 2_400_000, 2_700_000, 3_200_000,
    ],
    days_elapsed: 14,
    days_in_period: 31,
    projected_micros: 62_000_000,
    projected_low_micros: 54_000_000,
    projected_high_micros: 70_000_000,
    cap_micros: 80_000_000,
    on_track: true,
    headroom_days: 9,
    period: "month",
    period_start: "2026-07-01",
    as_of: "2026-07-14",
    period_end: "2026-07-31",
  },
  coverage: {
    status: "green",
    has_proxy: true,
    has_otel: true,
    blind_sources: [],
    sources: [
      { source: "proxy", steps: 12, last_day: "2026-07-15", heartbeat: false },
      { source: "otel-event", steps: 8, last_day: "2026-07-15", heartbeat: true },
    ],
  },
  anomalies: [
    {
      date: "2026-07-14",
      series_key: "claude-sonnet-4",
      kind: "spike",
      value_micros: 6_400_000,
      baseline_micros: 2_000_000,
    },
  ],
  cost_regressions: [],
  punchcard: { cells: [], max_micros: 0, total_micros: 0 },
  heatmap: { cells: [], max_micros: 0, total_micros: 0, weeks: 0 },
  budget: {
    period: "month",
    spent_micros: 28_000_000,
    cap_micros: 80_000_000,
    warn_pct: 80,
    pct: 35,
    status: "ok",
  },
  failures: {
    rows: [],
    total_micros: 0,
    total_failed_steps: 0,
    pct_of_spend: 0,
    pricing_version: "fixture",
    estimated: true,
  },
  loops: {
    rows: [],
    total_micros: 0,
    total_redundant_steps: 0,
    pricing_version: "fixture",
    estimated: true,
  },
  savings: {
    opportunities: [],
    opportunities_v2: [],
    total_recoverable_micros: 0,
    total_spend_micros: 28_000_000,
    savings_index: 100,
    pricing_version: "fixture",
    estimated: true,
  },
  sessions_live: [],
  sessions: { rows: [], total_micros: 0, pricing_version: "fixture", estimated: true },
  correlate: {
    rows: [
      {
        run_id: "run-profile-fixture",
        model: "claude-sonnet-4",
        cache_control: true,
        effort: "high",
        ttl: "5m",
        cost_micros: 2_400_000,
        tokens: 12_000,
        duration_ms: 430,
        steps: 2,
      },
    ],
    pricing_version: "fixture",
    estimated: true,
  },
  lineage: [],
  units: {
    rows: [],
    unbucketed_runs: 0,
    total_micros: 0,
    pricing_version: "fixture",
    estimated: true,
  },
  recent_steps: [],
  advise: [],
  whatif: {
    baseline_micros: 0,
    recommendations: [],
    estimated: true,
    approximate: true,
  },
  pricing: {
    version: "fixture",
    effective_date: "2026-07-01",
    note: "Bundled deterministic fixture.",
    models: [],
  },
  confidence: {
    pricing_age_days: 15,
    unpriced_token_share_pct: 2,
    coverage_status: "unknown",
    label: "medium",
    estimated: true,
  },
  reconcile: {
    day: "2026-07-15",
    pricing_version: "fixture",
    rows: [],
    estimate_total_micros: 0,
    vendor_total_micros: 0,
    delta_total_micros: 0,
    has_vendor: false,
  },
  otlp_status: {
    listening: true,
    port: 4318,
    events: 8,
    last_event_unix: 1_752_576_000,
    age_seconds: 3,
    hooks_seen: 2,
    hooks: [],
  },
  resolve: {
    data: {
      run_ids: COHORT_ENTITIES.map((row) => row.entity.run_id),
      run_count: 1_000,
      step_count: 2_000,
      total_micros: COHORT_TOTAL_MICROS,
      entity_rows: COHORT_ENTITIES,
    },
    provenance: PROVENANCE,
  },
  facets: {
    data: {
      dimension: "model",
      rows: [
        {
          value: "claude-sonnet-4",
          selection_support: 920,
          baseline_support: 410,
          selection_micros: 410_000_000,
          baseline_micros: 160_000_000,
          selection_support_share_pct: 92,
          baseline_support_share_pct: 41,
          selection_spend_share_pct: 88,
          baseline_spend_share_pct: 38,
          delta_support_share_points: 51,
          lift_ratio: 2.24,
          selection_missing_pct: 1,
          baseline_missing_pct: 3,
        },
      ],
    },
    provenance: PROVENANCE,
  },
  compare: {
    data: {
      selection: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
      baseline: { run_ids: [], run_count: 0, step_count: 0, total_micros: 0, entity_rows: [] },
      total_delta_micros: 0,
      volume_delta_micros: 0,
      size_delta_micros: 0,
      efficiency_delta_micros: 0,
      compatibility_warnings: [],
    },
    provenance: PROVENANCE,
  },
  search: {
    data: { entities: [{ run_id: "run-profile-fixture" }], truncated: false },
    provenance: PROVENANCE,
  },
  run_status: {
    run_id: "run-profile-fixture",
    micros: 2_400_000,
    steps: 2,
    tokens: 12_000,
    last_model: "claude-sonnet-4",
    top_cause: "fresh_input",
  },
  run_statuses: [
    {
      run_id: "run-profile-fixture",
      micros: 2_400_000,
      steps: 2,
      tokens: 12_000,
      last_model: "claude-sonnet-4",
      top_cause: "fresh_input",
    },
  ],
  run_meta: {
    run_id: "run-profile-fixture",
    created_date: "2026-07-14",
    privacy_policy_id: "strict_counts",
    profile: "strict_counts",
    steps: 2,
    models: ["claude-sonnet-4"],
    providers: ["anthropic"],
    sources: ["otel"],
    stop_reasons: ["end_turn"],
    pricing_version: "fixture",
    effective_date: "2026-07-01",
  },
  run_steps: [
    {
      ordinal: 1,
      provider: "anthropic",
      model: "claude-sonnet-4",
      fresh_input: 6_000,
      cache_read: 0,
      cache_write: 0,
      output: 800,
      reasoning: 0,
      tokens: 6_800,
      micros: 1_600_000,
      stop_reason: "tool_use",
      duration_ms: 250,
      start_unix_nano: "1784073600000000000",
      end_unix_nano: "1784073600250000000",
      trace_id: "fixture-trace",
      span_id: "fixture-span-1",
      parent_span_id: "fixture-root",
      anatomy: {
        components: [{ component: "system", label: "System", bytes: 1_200, cached: false }],
        total_bytes: 1_200,
        system_hash: "opaque-system-hash",
      },
    },
    {
      ordinal: 2,
      provider: "anthropic",
      model: "claude-sonnet-4",
      fresh_input: 2_000,
      cache_read: 2_500,
      cache_write: 0,
      output: 700,
      reasoning: 0,
      tokens: 5_200,
      micros: 800_000,
      stop_reason: "end_turn",
      duration_ms: 180,
      start_unix_nano: "1784073600100000000",
      end_unix_nano: "1784073600280000000",
      trace_id: "fixture-trace",
      span_id: "fixture-span-2",
      parent_span_id: "fixture-root",
    },
  ],
  session_autopsy: {
    run_id: "run-profile-fixture",
    total_micros: 2_400_000,
    classes: [
      { class: "fresh_input", micros: 1_500_000 },
      { class: "cache_read", micros: 200_000 },
      { class: "cache_write", micros: 100_000 },
      { class: "output", micros: 600_000 },
    ],
    reasoning_micros: 0,
    cache_hit_pct: 24,
    vs_median_pct: 160,
    fidelity: "component",
    headline: { kind: "efficient" },
    opportunities: [],
  },
  profile: {
    run_id: "run-profile-fixture",
    pricing_version: "fixture",
    sort: "cum",
    total_micros: 2_400_000,
    total_tokens: 12_000,
    rows: [
      { name: "run", self_micros: 0, cum_micros: 2_400_000, self_tokens: 0, cum_tokens: 12_000 },
      { name: "step 1 · claude-sonnet-4", self_micros: 0, cum_micros: 1_600_000, self_tokens: 0, cum_tokens: 6_800 },
      { name: "step 2 · claude-sonnet-4", self_micros: 0, cum_micros: 800_000, self_tokens: 0, cum_tokens: 5_200 },
      { name: "system", self_micros: 1_500_000, cum_micros: 1_500_000, self_tokens: 6_000, cum_tokens: 6_000 },
      ...Array.from({ length: 28 }, (_, index) => ({
        name: `component-${String(index + 1).padStart(2, "0")}`,
        self_micros: 20_000 + index,
        cum_micros: 400_000 - index * 5_000,
        self_tokens: 100 + index,
        cum_tokens: 300 + index,
      })),
    ],
  },
  flamegraph: {
    run_id: "run-profile-fixture",
    pricing_version: "fixture",
    effective_date: "2026-07-01",
    root: {
      name: "run run-profile-fixture",
      tokens: 12_000,
      micros: 2_400_000,
      children: [
        {
          name: "step 1 · claude-sonnet-4",
          tokens: 6_800,
          micros: 1_600_000,
          children: [
            {
              name: "System",
              tokens: 6_000,
              micros: 1_500_000,
              children: [{ name: "fresh", tokens: 6_000, micros: 1_500_000, cache_class: "fresh", children: [] }],
            },
            {
              name: "Output",
              tokens: 800,
              micros: 100_000,
              children: [{ name: "output", tokens: 800, micros: 100_000, cache_class: "output", children: [] }],
            },
          ],
        },
        {
          name: "step 2 · claude-sonnet-4",
          tokens: 5_200,
          micros: 800_000,
          children: [
            {
              name: "Conversation",
              tokens: 4_500,
              micros: 600_000,
              children: [{ name: "cache read", tokens: 4_500, micros: 600_000, cache_class: "cache_read", children: [] }],
            },
            {
              name: "Output",
              tokens: 700,
              micros: 200_000,
              children: [{ name: "output", tokens: 700, micros: 200_000, cache_class: "output", children: [] }],
            },
          ],
        },
      ],
    },
  },
  run_note: null,
  frontier: {
    points: [{ run_id: "run-profile-fixture", cost_micros: 2_400_000, quality: 91, on_frontier: true }],
    has_quality: true,
    pricing_version: "fixture",
    estimated: true,
  },
};

export interface FixtureOptions {
  /// Remove all persisted timing bounds so Timeline must honestly fall back to captured Step order.
  stepOrderOnly?: boolean;
  /// Observe API paths without replacing the fixture (used to prove counts-only never fetches bodies).
  onApiRequest?: (pathname: string) => void;
  /// Enable the mutable, local-only action, capture, and scenario state used by workflow journeys.
  mutableWorkflows?: boolean;
}

export interface WorkflowRequestLog {
  accept: Array<Record<string, unknown>>;
  dismiss: Array<Record<string, unknown>>;
  unaccept: Array<Record<string, unknown>>;
  verify: Array<Record<string, unknown>>;
  experiment: Array<Record<string, unknown>>;
}

export interface FixtureController {
  /// Model the user/service remedy landing before Capture's read-only self-check is rerun.
  recoverCapture(): void;
  /// Advance the deterministic stored cohort from an incomplete to a complete after-window.
  completeVerification(): void;
  requestLog(): WorkflowRequestLog;
}

interface MutableFixtureState {
  captureFlowing: boolean;
  verificationComplete: boolean;
  action: Record<string, unknown> | null;
  requests: WorkflowRequestLog;
}

function mutableWorkflowBody(pathname: string, method: string, state: MutableFixtureState): unknown | undefined {
  if (pathname === "/__tare/savings" && method === "GET") return WORKFLOW_SAVINGS;
  if (pathname === "/__tare/savings/actions" && method === "GET") {
    return state.action ? [state.action] : [];
  }
  if (pathname === "/__tare/coverage") {
    return state.captureFlowing
      ? {
          status: "green",
          has_proxy: false,
          has_otel: true,
          blind_sources: [],
          sources: [
            { source: "otel-event", steps: 12, last_day: "2026-07-16", heartbeat: true },
          ],
        }
      : {
          status: "red",
          has_proxy: false,
          has_otel: false,
          blind_sources: ["codex"],
          sources: [{ source: "codex", steps: 0, last_day: "", heartbeat: true }],
        };
  }
  if (pathname === "/__tare/sessions_live") {
    return state.captureFlowing
      ? [
          {
            session: "fixture-live",
            source: "codex",
            state: "working",
            last_seen_age_s: 2,
            events: 12,
            last_model: "gpt-5",
            micros: 1_200_000,
          },
        ]
      : [
          {
            session: "fixture-stopped",
            source: "codex",
            state: "waiting",
            last_seen_age_s: 4,
            events: 0,
            last_model: null,
            micros: 0,
          },
        ];
  }
  if (pathname === "/__tare/otlp_status") {
    return state.captureFlowing
      ? {
          listening: true,
          port: 4318,
          events: 12,
          last_event_unix: 1_752_662_400,
          age_seconds: 2,
          hooks_seen: 2,
          hooks: [],
        }
      : {
          listening: false,
          port: 4318,
          events: 0,
          last_event_unix: 0,
          age_seconds: null,
          hooks_seen: 0,
          hooks: [],
        };
  }
  if (pathname === "/__tare/pricing") {
    return {
      version: "2026.07.10",
      effective_date: "2026-07-10",
      note: "Bundled locally for the deterministic workflow fixture.",
      models: [
        {
          provider: "anthropic",
          model_id: "claude-haiku-4-5",
          tier: "standard",
          input_micro_per_mtok: 1_000_000,
          output_micro_per_mtok: 5_000_000,
          cache_read_micro_per_mtok: 100_000,
          cache_write_5m_micro_per_mtok: 1_250_000,
          cache_write_1h_micro_per_mtok: 2_000_000,
        },
      ],
    };
  }
  if (pathname === "/__tare/confidence") {
    return {
      pricing_age_days: 6,
      unpriced_token_share_pct: 18,
      coverage_status: "unknown",
      label: "medium",
      estimated: true,
    };
  }
  if (pathname === "/__tare/reconcile") {
    return {
      day: "2026-07-15",
      pricing_version: "2026.07.10",
      rows: [],
      estimate_total_micros: 0,
      vendor_total_micros: 0,
      delta_total_micros: 0,
      has_vendor: false,
    };
  }
  if (pathname === "/__tare/report") {
    return {
      total_micros: 10_000_000,
      causes: [],
      unpriced: [{ provider: "local", model: "future-model", token_total: 9_000 }],
      pricing_version: "2026.07.10",
      effective_date: "2026-07-10",
      estimated: true,
    };
  }
  if (pathname === "/__tare/sessions") {
    return {
      rows: [{ run_id: "run-profile-fixture" }],
      total_micros: 2_400_000,
      pricing_version: "2026.07.10",
      estimated: true,
    };
  }
  if (pathname === "/__tare/investigations" && method === "GET") {
    return [WORKFLOW_INVESTIGATION];
  }
  if (pathname === "/__tare/whatif") {
    return {
      baseline_micros: 10_000_000,
      recommendations: [
        {
          to_provider: "anthropic",
          to_model: "claude-haiku-4-5",
          total_after_micros: 4_000_000,
          delta_micros: -6_000_000,
          approximate_tokenizer: true,
          approximate_cross_provider: false,
        },
      ],
      estimated: true,
      approximate: true,
    };
  }
  if (pathname === "/__tare/experiment" && method === "POST") return WORKFLOW_EXPERIMENT;
  return undefined;
}

/// Install the fixture interceptor. Call at the top of a test (or in a beforeEach) BEFORE navigation.
export async function installFixtures(page: Page, options: FixtureOptions = {}): Promise<FixtureController> {
  const mutableState: MutableFixtureState = {
    captureFlowing: false,
    verificationComplete: false,
    action: null,
    requests: { accept: [], dismiss: [], unaccept: [], verify: [], experiment: [] },
  };
  await page.route("**/__tare/**", async (route: Route) => {
    const url = new URL(route.request().url());
    const method = route.request().method();
    const last = url.pathname.split("/").filter(Boolean).pop() ?? "";
    options.onApiRequest?.(url.pathname);
    let body = options.mutableWorkflows ? mutableWorkflowBody(url.pathname, method, mutableState) : undefined;
    if (options.mutableWorkflows && method === "POST") {
      let request: Record<string, unknown> = {};
      try {
        request = route.request().postDataJSON() as Record<string, unknown>;
      } catch {
        // A malformed request still receives a deterministic response; the journey assertions fail
        // on the recorded empty body instead of leaking to a live service.
      }
      if (url.pathname === "/__tare/savings/accept" || url.pathname === "/__tare/savings/dismiss") {
        const target = url.pathname.endsWith("accept") ? mutableState.requests.accept : mutableState.requests.dismiss;
        target.push(request);
        const cohort = request.cohort as typeof WORKFLOW_COHORT;
        mutableState.action = {
          ...request,
          cohort_hash: cohortHash(cohort),
          status: url.pathname.endsWith("accept") ? "applied" : "dismissed",
          acted_at: "2026-07-08T12:00:00Z",
          compatibility_warnings: [],
        };
        body = { ok: true };
      } else if (url.pathname === "/__tare/savings/unaccept") {
        mutableState.requests.unaccept.push(request);
        mutableState.action = null;
        mutableState.verificationComplete = false;
        body = { ok: true };
      } else if (url.pathname === "/__tare/savings/verify") {
        mutableState.requests.verify.push(request);
        body = {
          status: mutableState.verificationComplete ? "observed_reduction" : "verifying",
          complete: mutableState.verificationComplete,
          selection_before_micros: 10_000_000,
          selection_after_micros: 8_000_000,
          baseline_before_micros: 5_000_000,
          baseline_after_micros: 5_000_000,
          observed_reduction_micros: 2_000_000,
          matched_before: 4,
          matched_after: 4,
          unmatched_before: 0,
          unmatched_after: 0,
          baseline_matched_before: 4,
          baseline_matched_after: 4,
          baseline_unmatched_before: 0,
          baseline_unmatched_after: 0,
          compatibility_warnings: ["Observed association does not by itself establish causality."],
        };
      } else if (url.pathname === "/__tare/experiment") {
        mutableState.requests.experiment.push(request);
      }
    }
    if (body === undefined) {
      // Timeline and Compare endpoints echo the brushed window or navigated run pair.
      let request: Record<string, unknown> = {};
      if (method === "POST") {
        try {
          request = route.request().postDataJSON() as Record<string, unknown>;
        } catch {
          /* malformed request → deterministic empty body, never a live call */
        }
      }
      body = analysisBody(url.pathname, method, request, url);
    }
    if (body === undefined) body = last in BY_PATH ? BY_PATH[last] : [];
    if (options.stepOrderOnly && last === "run_steps") {
      body = (body as Array<Record<string, unknown>>).map((step) =>
        Object.fromEntries(
          Object.entries(step).filter(([key]) => key !== "start_unix_nano" && key !== "end_unix_nano")
        )
      );
    }
    return route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  });
  return {
    recoverCapture: () => {
      mutableState.captureFlowing = true;
    },
    completeVerification: () => {
      mutableState.verificationComplete = true;
    },
    requestLog: () => mutableState.requests,
  };
}
