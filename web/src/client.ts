// The data the web app needs from the host (Tauri `invoke`, an HTTP endpoint, or a fake
// in tests). Keeping it an interface lets the UI be driven headlessly under jsdom.

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

interface TrimRow {
  cause: string;
  detail: string;
  tokens: number;
  micros: number;
  projected_saved_micros: number;
}

interface UnpricedModel {
  provider: string;
  model: string;
  token_total: number;
  step_count: number;
}

/// Report DTO returned by the host and consumed by pricing and export flows.
export interface Report {
  pricing_version: string;
  effective_date: string;
  estimated: boolean;
  total_micros: number;
  rows: TrimRow[];
  unpriced?: UnpricedModel[];
  /// `coarse` when capture did not include prompt-component detail.
  attribution_confidence?: string;
}

export interface TrendQuery {
  from?: string;
  to?: string;
  by?: string; // total | provider | model | cause
  window?: number;
  threshold?: number;
}

export interface Anomaly {
  date: string;
  series_key: string;
  kind: string;
  value_micros: number;
  baseline_micros: number;
  /// minor | material | major — impact × share-of-day severity. Optional (older serve).
  materiality?: string;
}

export interface TodaySpend {
  run_count?: number;
  total_micros: number;
  pricing_version: string;
  effective_date: string;
}

/// The single cost-class that drove > half of a spike's delta; absent = diffuse change.
export interface SpikeCause {
  label: string;
  delta_micros: number;
  share_pct: number;
}

/// Deterministic volume × size × efficiency decomposition of one spend spike (tare-core anomaly.rs).
/// The three deltas sum EXACTLY to `total_delta_micros`. Estimate, on-device, no guessing.
export interface AnomalyWhy {
  series_key: string;
  total_delta_micros: number;
  volume_micros: number;
  size_micros: number;
  efficiency_micros: number;
  headline: string;
  bisect_hint: string;
  dominant_cause?: SpikeCause;
}

/// Scoped anomaly-explanation request. When `scope` is present the cohort is resolved to its
/// run set BEFORE detection (not a post-filter). `dimension` uses the short forms — NOT the trend
/// wire tags. `window`/`threshold` default to 7 / 50 server-side when omitted.
export interface AnomalyWhyRequest {
  from?: string;
  to?: string;
  dimension: "total" | "provider" | "model" | "cause";
  window?: number;
  threshold?: number;
  scope?: CohortSpec;
}

/// One merged flame-diff node: A's and B's cost side by side plus the signed delta (tare-core
/// flame_diff.rs). `delta_micros = micros_b − micros_a`; `delta_bps` is the share-of-tree delta the
/// normalized view colors by. This is the NODE-level diff — distinct from the row-level `ReportDiff`.
export interface FlameDiffNode {
  name: string;
  tokens_a: number;
  tokens_b: number;
  micros_a: number;
  micros_b: number;
  delta_micros: number;
  share_a_bps: number;
  share_b_bps: number;
  delta_bps: number;
  cache_class?: string;
  children: FlameDiffNode[];
}

/// Hierarchical node-level diff over an explicit run pair. `normalized` = share-mode (structural)
/// diffing, so two different-sized runs compare by proportion rather than absolute dollars.
export interface FlameDiffModel {
  run_a: string;
  run_b: string;
  normalized: boolean;
  total_a_micros: number;
  total_b_micros: number;
  root: FlameDiffNode;
}

/// One step in a run's chronological timeline (counts + cost; no payload).
export interface RunStep {
  ordinal: number;
  provider: string;
  model: string;
  fresh_input: number;
  cache_read: number;
  cache_write: number;
  output: number;
  reasoning: number;
  tokens: number;
  micros: number;
  stop_reason: string | null;
  /// Observed local round-trip latency (ms) measured at the capture edge; 0/absent = unmeasured or
  /// the privacy opt-out. NOT a provider SLA.
  duration_ms?: number;
  /// OTLP wall-clock timing/span identity, present only for out-of-band capture
  /// that carried real timestamps/spans. Unix NANOSECONDS as decimal STRINGS (never JS numbers —
  /// they exceed 2^53). When `start_unix_nano` is absent, the UI shows STEP ORDER, not a timeline.
  /// `end_unix_nano` is derived from start + duration. `parent_span_id` is the ONLY evidence for a
  /// parent/concurrency relationship — never infer concurrency without it.
  start_unix_nano?: string;
  end_unix_nano?: string;
  trace_id?: string;
  span_id?: string;
  parent_span_id?: string;
  /// Privacy-safe prompt anatomy — component byte-weights + a stable system-hash chip.
  anatomy?: PromptAnatomy;
}

/// One step's captured, redacted request/response bodies (Inspect layer). Present ONLY
/// under the `max_inspect` privacy profile; both strings are already secret-scrubbed and length-
/// capped at the capture edge (they are NOT raw payloads). `truncated` marks a body clipped at the
/// capture cap so the UI can say so honestly rather than implying the whole body is shown.
export interface TranscriptStep {
  req: string;
  resp: string;
  /// `null` only for legacy rows captured before truncation evidence was persisted.
  truncated: boolean | null;
}

/// A step's structural prompt anatomy: counts/labels only, never prompt text.
export interface PromptAnatomy {
  components: Array<{ component: string; label: string; bytes: number; cached: boolean }>;
  total_bytes: number;
  system_hash: string | null;
  request_hash?: string | null;
  stream?: boolean;
  cache_control?: boolean;
  ttl?: "5m" | "1h";
  effort?: string | null;
}

/// A step in the cross-run live activity tail — a RunStep plus the run it belongs to.
export interface RecentStep extends RunStep {
  run_id: string;
}

/// Recorded provenance for a run — makes the anonymous flamegraph self-describing.
export interface RunMeta {
  run_id: string;
  created_date: string;
  privacy_policy_id: string | null;
  profile: string | null;
  steps: number;
  models: string[];
  providers: string[];
  sources: string[]; // capture sources: proxy | otel-span | otel-event | unknown
  stop_reasons: string[];
  pricing_version: string;
  effective_date: string;
}

export type BurnRateRange = "day" | "week" | "month" | "year";

/// Clock-free burn-rate projection for a calendar-to-date range. "At your captured
/// pace" — never a real-time forecast. An omitted range retains the configured budget period.
export interface BurnRate {
  /** Median captured spend on non-zero days (an active-day statistic, not a calendar-day mean). */
  run_rate_micros_per_day: number;
  /** Activity-adjusted calendar-day rate actually used for the forward projection. */
  effective_rate_micros_per_day: number;
  /** Actual captured period-to-date spend. */
  spent_micros: number;
  active_days: number;
  /** Dense actual daily spend from `period_start` through `as_of`. */
  daily_spend_micros: number[];
  days_elapsed: number;
  days_in_period: number;
  projected_micros: number;
  projected_low_micros: number; // p25-pace projection (band low)
  projected_high_micros: number; // p75-pace projection (band high)
  cap_micros: number;
  on_track: boolean;
  headroom_days: number | null;
  period: BurnRateRange;
  period_start: string; // YYYY-MM-DD
  as_of: string; // YYYY-MM-DD
  period_end: string; // YYYY-MM-DD
}

/// Capture-coverage / blind-spot report: whether DATA is flowing from each channel.
export interface CoverageSource {
  source: string; // proxy | otel-event | otel-span | unknown
  steps: number;
  last_day: string;
  heartbeat: boolean; // seen in session_activity heartbeats
}
export interface Coverage {
  status: "green" | "amber" | "red" | "none";
  has_proxy: boolean;
  has_otel: boolean;
  blind_sources: string[]; // heartbeats but no cost steps (degraded capture / blind spend)
  sources: CoverageSource[];
}

export interface RunStatus {
  run_id: string;
  micros: number;
  steps: number;
  top_cause: string | null;
  /// Optional (newer serve): total tokens + the run's last model — for the managed Runs table.
  tokens?: number;
  last_model?: string;
  /// Lifecycle flags for the run state pill: a token-bearing step had no price; the
  /// terminal step was a retry-worthy failure.
  unpriced?: boolean;
  errored?: boolean;
}

export interface CacheAdvice {
  /** Added by provider-aware producers; absent on older compatible backends. */
  provider?: string;
  model: string;
  system_tokens: number;
  sends: number;
  uncached_micros: number;
  cached_5m_micros: number;
  cached_1h_micros: number;
  save_5m_micros: number;
  save_1h_micros: number;
  recommend: string; // "5m" | "1h" | "none"
  breakeven_reads: number | null;
}

export interface WhatIfRecommendation {
  to_provider: string;
  to_model: string;
  total_after_micros: number;
  delta_micros: number;
  approximate_tokenizer: boolean;
  approximate_cross_provider: boolean;
}

export interface WhatIfRecommendations {
  baseline_micros: number;
  recommendations: WhatIfRecommendation[];
  estimated: boolean;
  approximate: boolean;
}

export interface CauseDelta {
  cause: string;
  micros_before: number;
  micros_after: number;
  delta_micros: number;
}

export interface ReportDiff {
  pricing_version: string;
  estimated: boolean;
  total_before: number;
  total_after: number;
  delta_micros: number;
  rows: CauseDelta[];
}

/// One model's bundled rates (micro-USD per 1M tokens), for the Models/Pricing catalog.
export interface ModelRate {
  provider: string;
  model_id: string;
  tier: string;
  input_micro_per_mtok: number;
  output_micro_per_mtok: number;
  cache_read_micro_per_mtok: number;
  cache_write_5m_micro_per_mtok: number;
  cache_write_1h_micro_per_mtok: number;
}

export interface PricingInfo {
  version: string;
  effective_date: string;
  note: string | null;
  /// provider id -> number of priced models in the bundled table. Optional (older serve).
  models_by_provider?: Record<string, number>;
  /// Per-model rate rows for the catalog screen. Optional (older serve).
  models?: ModelRate[];
}

export interface ReceiptResult {
  receipt: unknown;
  verify: {
    scope: string;
    pricing_version: string;
    recomputed_total_micros: number;
    rows: number;
    flamegraph_checked: boolean;
    digest: number;
  };
}

export interface ProxyStatus {
  running: boolean;
  port: number;
  url: string;
}

/// Liveness of the out-of-band OTLP receiver for the Capture utility.
/// One lifecycle hook's health: how many have fired and how long since the last.
export interface HookHealth {
  event: string;
  count: number;
  age_seconds: number;
}
export interface OtlpStatus {
  listening: boolean;
  port: number;
  events: number;
  last_event_unix: number;
  age_seconds: number | null;
  /// Total lifecycle-hook events received; 0 = hooks not wired/firing.
  hooks_seen?: number;
  /// Per-event hook-health rows.
  hooks?: HookHealth[];
}

export interface RollupRow {
  label: string;
  runs: number;
  steps: number;
  tokens: number;
  micros: number;
  micros_per_call: number;
  /// Run ids in this bucket (sorted), for the group-by drill-down. Absent on older serve.
  members?: string[];
}

export interface RollupReport {
  dimension: string;
  rows: RollupRow[];
  total_micros: number;
  pricing_version: string;
  estimated: boolean;
}

/// A parent-bucket restriction for the progressive drill: bucket by the requested
/// dim but only over steps whose `by`-dim label equals `label`. `by` is a RollupDim name
/// (template/session/step/tool/…); the filtered rows reconcile to the filtered total.
export interface RollupFilter {
  by: string;
  label: string;
}

/// One (weekday, hour) bucket of the day×hour punchcard. `weekday` 0=Mon…6=Sun,
/// `hour` 0–23, `level` a 0–4 intensity scaled to the busiest bucket.
export interface PunchCell {
  weekday: number;
  hour: number;
  micros: number;
  level: number;
}

/// The full 7×24 spend punchcard — `cells` is always the complete rectangle (168, row-major).
export interface PunchcardModel {
  cells: PunchCell[];
  max_micros: number;
  total_micros: number;
}

/// One day of the calendar heatmap. `weekday` 0=Mon…6=Sun, `week` = column index,
/// `level` a 0–4 intensity scaled to the busiest day.
export interface HeatCell {
  date: string;
  micros: number;
  level: number;
  weekday: number;
  week: number;
}

/// The calendar heatmap over a window (sparse — only days present); `weeks` = column count.
export interface HeatmapModel {
  cells: HeatCell[];
  max_micros: number;
  total_micros: number;
  weeks: number;
}

export interface SessionRow {
  session: string;
  runs: number;
  steps: number;
  tokens: number;
  micros: number;
  tools: number;
  agents: number;
  micros_per_step: number;
  input_curve: number[];
  cache_erosion_turn: number | null;
}

export interface SessionReport {
  rows: SessionRow[];
  total_micros: number;
  pricing_version: string;
  estimated: boolean;
}

/// One run projected onto config-knob + outcome axes (the "Explain" panel).
export interface CorrelationRow {
  run_id: string;
  model: string;
  cache_control: boolean;
  effort: string | null;
  ttl: string;
  cost_micros: number;
  tokens: number;
  duration_ms: number;
  steps: number;
}

export interface CorrelationReport {
  rows: CorrelationRow[];
  pricing_version: string;
  estimated: boolean;
}

/// One version in a prompt/config lineage projected onto cost-per-run.
export interface LineageVersionRow {
  label: string;
  hash: number;
  runs: number;
  steps: number;
  tokens: number;
  cost_micros: number;
  micros_per_run: number;
}

export interface LineageReport {
  name: string;
  rows: LineageVersionRow[];
  pricing_version: string;
  estimated: boolean;
}

/// One unit of work projected onto its cost denominator.
export interface UnitRow {
  name: string;
  runs: number;
  steps: number;
  tokens: number;
  cost_micros: number;
  micros_per_run: number;
}

export interface UnitReport {
  rows: UnitRow[];
  unbucketed_runs: number;
  total_micros: number;
  pricing_version: string;
  estimated: boolean;
}

export interface LoopWasteRow {
  label: string;
  redundant_steps: number;
  tokens: number;
  micros: number;
  max_repeat: number;
}

export interface LoopWasteReport {
  rows: LoopWasteRow[];
  total_micros: number;
  total_redundant_steps: number;
  pricing_version: string;
  estimated: boolean;
}

export interface FailureWasteRow {
  label: string;
  failed_steps: number;
  tokens: number;
  micros: number;
}

export interface FailureWasteReport {
  rows: FailureWasteRow[];
  total_micros: number;
  total_failed_steps: number;
  pct_of_spend: number;
  pricing_version: string;
  estimated: boolean;
}

export interface SessionLive {
  session: string;
  source: string;
  state: string; // "working" | "idle" | "ended"
  last_seen_age_s: number;
  events: number;
  last_model: string;
  micros: number; // lifetime estimated spend (enriched from the store)
  /// Latest activity: "responded" | "user_prompt" | "tool_result" | "" — working-vs-waiting hint.
  phase?: string;
}

/// Claude Code's OWN reported spend/tokens for today (from its OTLP metrics) — a cross-check
/// against Tare's step-derived estimate, never added to it.
export interface VendorToday {
  cost_micros: number;
  tokens: number;
  day: string;
  available: boolean; // false when no Claude Code metrics were captured (hide the cross-check)
}

/// One model's row in the estimate-vs-Claude Code reconciliation. Estimate is primary;
/// the reported metric is a cross-check, never merged.
export interface ReconcileRow {
  model: string;
  estimate_micros: number;
  vendor_micros: number;
  delta_micros: number; // estimate - vendor (signed)
  tokens: number;
  cause: "ok" | "unpriced" | "coverage_gap" | "estimate_only" | "mismatch";
}

export interface Reconciliation {
  day: string;
  pricing_version: string;
  rows: ReconcileRow[];
  estimate_total_micros: number;
  vendor_total_micros: number;
  delta_total_micros: number;
  has_vendor: boolean; // false when Claude Code reported nothing (hide the panel)
}

/// One recoverable-spend opportunity in the unified Savings Ledger.
export interface Opportunity {
  kind: string; // loop | failure | cache | model-swap
  label: string;
  recoverable_micros: number;
  confidence: string; // measured | projected | approximate
  fix_text: string; // copy-pasteable remediation
  effort: string; // S | M | L
}

/// A specific step an opportunity's evidence points at.
export interface OpportunityStepRef {
  run_id: string;
  step_ordinal: number;
}

/// v2 opportunity: the v1 fields plus a stable detector-versioned `opportunity_key`, capped
/// evidence refs with FULL counts (`evidence_truncated` when the inline lists were capped at 500),
/// the exact resolvable `cohort_snapshot`, honest `assumptions`, and an optional `quality_risk`.
/// The key derives from stable identity, so it survives display/copy changes.
export interface OpportunityV2 {
  opportunity_key: string;
  kind: string;
  label: string;
  recoverable_micros: number;
  confidence: string;
  fix_text: string;
  effort: string;
  affected_run_count: number;
  affected_step_count: number;
  affected_run_ids: string[];
  affected_steps: OpportunityStepRef[];
  evidence_truncated: boolean;
  evidence_method: string;
  cohort_snapshot: CohortSpec;
  assumptions: string[];
  quality_risk?: string;
}

/// The unified Savings Ledger — one ranked worklist of recoverable spend. The headline
/// `total_recoverable_micros` is CAPPED POTENTIAL savings (bounded by spend), not a deduplicated
/// non-overlapping floor: categories can overlap at the step level (honest labels).
/// The `opportunities_v2` / capped-potential / applied / observed fields are optional for backward
/// wire compatibility.
export interface SavingsLedger {
  opportunities: Opportunity[];
  total_recoverable_micros: number; // capped potential (≤ spend); NOT a deduped/non-overlapping floor
  total_spend_micros: number;
  savings_index: number; // 0..100
  pricing_version: string;
  estimated: boolean;
  opportunities_v2?: OpportunityV2[];
  capped_potential_micros?: number; // explicit honest alias of total_recoverable_micros
  applied_micros?: number; // Recoverable spend marked Applied; zero before any action.
  observed_micros?: number; // Observed post-application reduction; zero before verification.
}

/// One ranked, dollar-quantified thing to do. `basis` is "recoverable" (a fix would
/// save it) or "at-risk" (an upper-bound exposure, never a guaranteed saving).
export interface ActionItem {
  kind: string;
  label: string;
  dollars_micros: number;
  basis: string;
  confidence: string;
  action: string;
  effort: string;
}
/// The unified Action Plan: recoverable opportunities + at-risk advisories in one ranked list, with
/// the two totals kept separate (capped potential is never inflated by at-risk).
export interface ActionPlan {
  items: ActionItem[];
  recoverable_floor_micros: number;
  at_risk_total_micros: number;
  total_spend_micros: number;
  savings_index: number;
  pricing_version: string;
  estimated: boolean;
}

/// Reasoning/thinking-token breakout: output spend split into answer vs reasoning.
export interface ReasoningModel {
  model: string;
  reasoning_tokens: number;
  reasoning_micros: number;
  output_micros: number;
}
export interface ReasoningBreakout {
  reasoning_tokens: number;
  answer_tokens: number;
  reasoning_micros: number;
  output_micros: number;
  reasoning_pct: number; // reasoning as an integer % of output spend
  by_model: ReasoningModel[];
}

/// Flat/cum profile table: the pprof primitive over a run's flamegraph. `self` is
/// spend charged directly at a name (leaf spend; 0 for aggregator frames); `cum` is self +
/// descendants. Sum of `self_micros` across rows equals `total_micros`.
export type ProfileSort = "flat" | "cum";
export interface ProfileRow {
  name: string;
  self_micros: number;
  cum_micros: number;
  self_tokens: number;
  cum_tokens: number;
}
export interface ProfileTable {
  run_id: string;
  pricing_version: string;
  sort: ProfileSort;
  total_micros: number;
  total_tokens: number;
  rows: ProfileRow[];
}

/// Realized cache-savings ledger: money already kept because cache reads were billed
/// at the read rate instead of the base input rate they'd have cost as fresh input.
export interface CacheModelSaving {
  model: string;
  cache_read_tokens: number;
  saved_micros: number;
}
export interface CacheLedger {
  cache_read_tokens: number;
  saved_micros: number; // realized savings vs the 1× base input rate
  read_cost_micros: number; // what the reads actually cost (at the read rate)
  by_model: CacheModelSaving[];
}

/// Cost-effectiveness — dollars per outcome (the value side of the equation). Ratios are null
/// when their denominator is zero.
export interface Effectiveness {
  cost_micros: number;
  per_pull_request_micros: number | null;
  per_commit_micros: number | null;
  per_1k_loc_micros: number | null;
  per_active_hour_micros: number | null;
  per_session_micros: number | null;
  accept_rate_pct: number | null;
  cost_per_successful_run_micros: number | null;
  run_success_rate_pct: number | null;
  estimated: boolean;
}

/// A cost-EFFECTIVENESS regression: a day whose $/outcome ($/commit, else
/// $/accepted-edit) jumped above its own trailing baseline — the outcome-aware sibling of Anomaly.
export interface CostRegression {
  day: string;
  outcome: string; // "commit" | "accepted-edit"
  ratio_micros: number;
  baseline_micros: number;
  over_pct: number;
}

/// Estimate-Confidence: how much to trust the dollar figure (pricing freshness + unpriced +
/// coverage fused into a label). Coverage is an honest STATUS: out-of-band capture has
/// no defensible denominator, so it is `unknown` rather than a fabricated `100`; a numeric
/// `coverage_share_pct` is present only when a real denominator exists.
export interface EstimateConfidence {
  pricing_age_days: number;
  unpriced_token_share_pct: number;
  coverage_status: "unknown" | "partial" | "full";
  coverage_share_pct?: number;
  label: string; // high | medium | low
  estimated: boolean;
}

export interface PeriodBudget {
  period: string;
  spent_micros: number;
  cap_micros: number;
  warn_pct: number;
  pct: number;
  status: string; // "ok" | "warn" | "over"
}

export interface SandwichRun {
  run_id: string;
  micros: number;
  tokens: number;
}

export interface Sandwich {
  component: string;
  total_micros: number;
  runs: SandwichRun[];
  pricing_version: string;
  estimated: boolean;
}

export interface Lenses {
  input_micros: number;
  output_micros: number;
  cache_saved_micros: number;
  total_micros: number;
  calls: number;
  micros_per_call: number;
  /// Token counts (all steps, priced or not) for the efficiency lenses.
  total_tokens: number;
  output_tokens: number;
  input_tokens: number;
  cache_read_tokens: number;
  pricing_version: string;
  estimated: boolean;
}

export interface TareConfigDto {
  budget: {
    max_spend_usd?: number;
    soft_spend_usd?: number;
    max_steps?: number;
    max_repeats?: number;
    period?: string;
    period_max_spend_usd?: number;
    warn_pct?: number;
  };
  privacy: { profile?: string; salt?: string; suppress_latency?: boolean; git_attribution?: boolean };
  providers: {
    anthropic_upstream?: string;
    openai_upstream?: string;
    gemini_upstream?: string;
    azure_openai_upstream?: string;
    bedrock_upstream?: string;
    /// User-supplied self-hosted cost overlay; empty/absent = local runs stay unpriced.
    local_overlay?: LocalOverlay[];
  };
  proxy: { port?: number; db?: string; otlp_port?: number; pricing?: string };
  /// Anomaly defaults + Vantage-style noise floors + acknowledged keys.
  /// Round-tripped in full so a Settings save (which edits only window/threshold) never wipes the
  /// floors or the acknowledged list.
  anomaly?: {
    window?: number;
    threshold?: number;
    acknowledged?: string[];
    dollar_floor_micros?: number;
    pct_of_daily_floor?: number;
    dedupe_window_days?: number;
  };
  ui?: { tz_offset_minutes?: number };
  /// When capture runs: "app_only" (default — only while the app is open, catches up on
  /// open), "always_on" (an OS login item keeps capturing across restarts), or "off".
  capture?: { mode?: string; jsonl?: boolean };
  /// Declarative alert rules; empty/absent keeps the built-in defaults.
  alert?: AlertRule[];
  /// Pricing settings: the reprice mode + per-model overrides. Round-tripped in full so
  /// a Settings save never wipes `[pricing]` — the UI edits `reprice` and preserves `overrides` as-is.
  pricing?: {
    reprice?: "as-of" | "latest";
    overrides?: Array<{
      model: string;
      input_usd_per_mtok: number;
      output_usd_per_mtok: number;
      cache_read_usd_per_mtok?: number;
    }>;
  };
  /// `[[unit]]` and `[[lineage]]` — work-unit and prompt-lineage rules (the config behind
  /// `tare unit` / `tare lineage`). No UI edits these, so they are typed as OPAQUE pass-through
  /// values on purpose: modelling their shape here would only create a second place to drift from
  /// the Rust structs, and the only requirement is that a save must not destroy them.
  ///
  /// Saving is a whole-file rewrite, so omitting these used to delete them from the user's
  /// tare.toml. `TareConfig::merge_json` now preserves any section a payload
  /// omits, so this is belt-and-braces — but keeping them on the DTO means a Settings save
  /// round-trips them explicitly instead of relying on the backend to notice they are missing.
  unit?: unknown[];
  lineage?: unknown[];
}

/// A declarative alert rule — mirrors the core `AlertRule`.
export interface AlertRule {
  metric: string; // today_spend | period_pct | run_rate | anomaly_kind
  threshold?: number;
  kind?: string;
  window_days?: number;
  min_events?: number;
}

/// A self-hosted cost overlay row — mirrors the core `LocalOverlay`. The effective rate
/// is `usd_per_mtok`, or `kwh_per_mtok * usd_per_kwh` when the direct price is absent.
export interface LocalOverlay {
  backend: string; // ollama | vllm | llama.cpp | tgi | …
  usd_per_mtok?: number;
  kwh_per_mtok?: number;
  usd_per_kwh?: number;
}

/// Per-session cost autopsy: exact per-class decomposition + the fallback-ladder headline
/// + actionability-gated waste opportunities for one session. All $ are measured; `headline` never
/// invents waste (see the Rust `autopsy` module). Adjacently tagged on `kind` from Rust.
export interface AutopsyClassCost {
  class: string; // fresh_input | cache_read | cache_write | output
  micros: number;
}
export type AutopsyHeadline =
  | { kind: "waste"; detail: Opportunity }
  | { kind: "structural_driver"; detail: { class: string; micros: number; vs_median_pct: number; lever: string } }
  | { kind: "efficient" };
export interface SessionAutopsy {
  run_id: string;
  total_micros: number;
  classes: AutopsyClassCost[];
  reasoning_micros: number; // sub-figure of the output class, never added to the total
  cache_hit_pct: number; // cache-read share of input tokens (high = good, cheap re-carry)
  vs_median_pct: number | null; // this session as % of the user's median session
  fidelity: string; // "component" (bodies seen) | "cost_class" (out-of-band: exact $ + waste, no within-prompt split)
  headline: AutopsyHeadline;
  opportunities: Opportunity[]; // actionable, gated, sorted desc by recoverable $
}

export interface TareClient {
  listRuns(): Promise<string[]>;
  flamegraph(runId: string): Promise<FlamegraphModel>;
  /// Flat/cum profile table (pprof primitive) over a run: self vs cumulative $ per name, ranked.
  profile(runId: string, sort?: ProfileSort, topN?: number): Promise<ProfileTable>;
  /// Cost×quality frontier across all runs: cost vs ingested quality, non-dominated set marked.
  frontier(): Promise<Frontier>;

  /// Attach a user-supplied quality scalar to a run — Tare STORES it, never grades it.
  /// Drives the frontier's quality axis; browser-wired so the frontier is exercisable without desktop.
  saveQuality(runId: string, score: number, source?: string): Promise<void>;
  report(): Promise<Report>;
  trend(query: TrendQuery): Promise<TrendReport>;
  anomalies(query?: TrendQuery): Promise<Anomaly[]>;
  /// Cost-effectiveness regressions: days whose $/outcome jumped above its trailing baseline.
  costRegressions(query?: { window?: number; threshold?: number }): Promise<CostRegression[]>;
  /// Seed the bundled sample run so onboarding can leave a first-run user on a populated Overview.
  /// Idempotent. Returns the seeded run id.
  seedDemo(): Promise<string>;
  today(): Promise<TodaySpend>;
  runStatus(runId: string): Promise<RunStatus>;
  /// Every run's status in one batched call — the Runs list uses this instead of a
  /// per-run `runStatus` round-trip. Each entry carries its own `run_id`.
  runStatuses(): Promise<RunStatus[]>;
  /// Recorded provenance for a run (created date, privacy, distinct dims, pricing version).
  runMeta(runId: string): Promise<RunMeta>;
  /// Ordered per-step timeline for a run (counts + cost), for the run Inspect tab.
  runSteps(runId: string): Promise<RunStep[]>;
  /// The redacted request/response bodies captured for one step, or `null` when nothing
  /// was captured (the default `strict_counts` profile stores no bodies — only `max_inspect` does).
  /// Bodies are already secret-scrubbed and length-capped at the capture edge.
  transcript(runId: string, step: number): Promise<TranscriptStep | null>;
  /// The live activity tail: the most recent steps across all runs, newest first.
  recentSteps(n?: number): Promise<RecentStep[]>;
  explain(runId: string): Promise<string>;
  /// Per-session cost autopsy for the run-detail drill. `median` = the user's median
  /// session cost (from the sessions list) for the vs-median reference; omit if unknown.
  sessionAutopsy(runId: string, median?: number): Promise<SessionAutopsy>;
  advise(): Promise<CacheAdvice[]>;
  /// The unified Savings Ledger: one ranked worklist of recoverable spend with fixes (headline is
  /// capped potential, not a deduped floor — see SavingsLedger).
  savings(): Promise<SavingsLedger>;
  /// The unified Action Plan, implemented by both browser and desktop transports.
  actionPlan(): Promise<ActionPlan>;
  /// Realized cache-savings ledger: money already kept from cache reads (the inverse of the trim-list).
  cacheLedger(): Promise<CacheLedger>;
  /// Reasoning/thinking-token breakout: output spend split into answer vs reasoning.
  reasoning(): Promise<ReasoningBreakout>;
  /// Cost-effectiveness: dollars per outcome (PR/commit/1k-LoC/active-hour/session) + accept-rate.
  effectiveness(): Promise<Effectiveness>;
  /// Estimate-Confidence: pricing freshness + unpriced + coverage fused into a trust label.
  confidence(): Promise<EstimateConfidence>;
  whatif(crossProvider: boolean): Promise<WhatIfRecommendations>;
  diff(a: string, b: string): Promise<ReportDiff>;
  /// Hierarchical node-level flame diff over an EXPLICIT run pair. Separate contract from
  /// `diff` (which is the row-level report diff). `normalized` selects share-mode structural diffing.
  /// Both transports delegate to the same shared store fn, so the model is equivalent over HTTP/Tauri.
  flameDiff(a: string, b: string, normalized?: boolean): Promise<FlameDiffModel>;
  pricing(): Promise<PricingInfo>;
  receipt(runId: string, maxPrivate?: boolean): Promise<ReceiptResult>;
  /// Spend bucketed by `by`. `filter` restricts to the steps under a parent bucket first — the
  /// progressive filter-preserving drill: `rollup("session", {by:"template", label})`
  /// answers "how did this template's spend split across sessions?". Omit for the unfiltered rollup.
  rollup(by: string, filter?: RollupFilter): Promise<RollupReport>;

  /// Day×hour spend punchcard: each captured run's priced cost bucketed by weekday×hour
  /// (from the hour stamped at ingest; hourless runs excluded). The "when do I burn tokens?" grid.
  punchcard(): Promise<PunchcardModel>;

  /// Calendar spend heatmap: daily priced cost as a weekday×week grid — the daily-return
  /// cadence view, companion to the punchcard.
  heatmap(): Promise<HeatmapModel>;
  /// Spend grouped by owning agent session/task (promotes the task above runs).
  sessions(): Promise<SessionReport>;
  /// Every run projected onto config-knob + outcome axes — the "Explain" correlation panel.
  correlate(): Promise<CorrelationReport>;
  /// Every configured prompt/config lineage projected onto cost-per-run.
  lineages(): Promise<LineageReport[]>;
  /// Captured runs bucketed into the configured units of work, cost per unit.
  units(): Promise<UnitReport>;
  /// Which agent sessions are running NOW (recency-derived liveness), most-recent first.
  sessionsLive(): Promise<SessionLive[]>;
  /// Claude Code's own reported spend today, as a cross-check. `available` is false when no
  /// Claude Code metrics were captured.
  vendorToday(): Promise<VendorToday>;
  /// Clock-free burn-rate projection. Omit the range to use the configured budget period.
  burnrate(range?: BurnRateRange): Promise<BurnRate>;
  /// Capture-coverage / blind-spot report (is data flowing from each channel?).
  coverage(): Promise<Coverage>;
  /// Per-model estimate-vs-Claude Code reconciliation for a day (default: today).
  reconcile(day?: string): Promise<Reconciliation>;
  /// Retry-loop waste (redundant identical re-issues) by offending tool/agent.
  loops(): Promise<LoopWasteReport>;
  /// Cost of errored/refused steps by offending tool/agent.
  failures(): Promise<FailureWasteReport>;
  /// Headline cost lenses for the overview scorecard (talk/listen split, cache savings, $/call).
  lenses(): Promise<Lenses>;
  /// Periodic (weekly/monthly) spend-budget status, if configured.
  budget(): Promise<PeriodBudget>;
  /// One prompt component's cost across all runs (the "sandwich" inspector).
  sandwich(component: string): Promise<Sandwich>;
  /// One run's export content (speedscope | otel | receipt) as a string.
  exportRun(runId: string, format: string): Promise<string>;
  config(): Promise<TareConfigDto>;
  /// Persist capture config. Rejects on the browser transport (read-only); desktop writes tare.toml.
  /// Write config back to tare.toml. `Partial` is deliberate: the backend merges the payload over
  /// what is on disk section by section (`TareConfig::merge_json`), so a caller sends only the
  /// sections it actually edits. Sending a section — even as `{}` — REPLACES it wholesale, so an
  /// empty object is how you clear one, never a harmless no-op.
  saveConfig(config: Partial<TareConfigDto>): Promise<void>;
  /// True if this transport can persist config (desktop only).
  canSaveConfig(): boolean;
  /// True if this transport can start/stop the capture proxy (desktop only).
  canControlProxy(): boolean;
  proxyStatus(): Promise<ProxyStatus>;
  proxyStart(port?: number): Promise<ProxyStatus>;
  proxyStop(): Promise<ProxyStatus>;
  /// True if this transport can raise an OS notification (desktop only).
  canNotify(): boolean;
  notify(title: string, body: string): Promise<void>;
  /// True if this transport can persist a window-close-to-background preference (desktop only). The
  /// browser has no window-close-to-tray, so it returns false and the Settings toggle is hidden.
  /// macOS is gated separately in the UI because it always keeps running.
  canBackgroundOnClose(): boolean;
  /// The persisted "keep Tare running in the background on close" preference (default false =
  /// exit-by-default). No-op transports resolve false.
  getBackgroundOnClose(): Promise<boolean>;
  /// Persist the "keep Tare running in the background on close" preference. Desktop writes
  /// background.json (read by the Rust close policy on the next close); browser transports no-op.
  setBackgroundOnClose(enabled: boolean): Promise<void>;
  /// Out-of-band OTLP receiver liveness. Implemented by both transports (HTTP reads
  /// `/__tare/otlp_status`; Tauri invokes the `otlp_status` command).
  otlpStatus(): Promise<OtlpStatus>;

  // ---- run notes: local-only user annotations ----
  /// The user's note for a run, or null if never annotated.
  getRunNote(runId: string): Promise<RunNote | null>;
  /// Upsert a run's note (tags/text/star). Loopback POST — the first browser→server mutation.
  saveRunNote(note: RunNote): Promise<void>;
  /// Purge a run's note.
  deleteRunNote(runId: string): Promise<void>;
  /// Run ids tagged with `tag` (for the Runs filter).
  runsByTag(tag: string): Promise<string[]>;
  /// Run ids the user has starred.
  starredRuns(): Promise<string[]>;
  /// Acknowledge an anomaly by its `date:series:kind` key — persists to [anomaly].acknowledged in
  /// tare.toml so it's hidden going forward. Loopback write.
  acknowledgeAnomaly(key: string): Promise<void>;
  /// Per-field config provenance: fieldPath → "env" | "tare.toml" | "default". Empty
  /// on transports that can't resolve it (the browser can't see server env).
  configOrigins(): Promise<Record<string, string>>;
  /// One-click opt-out: purge EVERY captured transcript from the separate transcript
  /// store. Idempotent — a no-op (not an error) when nothing was captured. Never touches the counts
  /// ledger. The privacy-max escape hatch that makes `max_inspect` safe to try.
  purgeTranscripts(): Promise<void>;

  // ---- Calibrated Bench cohort analysis ----
  // Every response is wrapped in AnalysisResponse{data,provenance}; HTTP and Tauri call the SAME
  // shared tare-cli cohort_*_json fns, so they return equivalent data for a given fixture. Both
  // transports implement all five; the fake mirrors the store engine for headless tests.
  /// Resolve a CohortSpec to its entity set + totals.
  resolveCohort(spec: CohortSpec): Promise<AnalysisResponse<CohortResolveResult>>;
  /// Selection-vs-baseline facet profile for one dimension.
  facetCohort(req: CohortFacetRequest): Promise<AnalysisResponse<CohortFacetResult>>;
  /// Decompose selection-vs-baseline spend into volume/size/efficiency deltas.
  compareCohort(req: CohortCompareRequest): Promise<AnalysisResponse<CohortCompareResult>>;
  /// Allow-listed cross-run search within a resolved cohort.
  searchCohort(req: CohortSearchRequest): Promise<AnalysisResponse<CohortSearchResult>>;
  /// Dense daily metrics over the exact cohort, with explicit unavailable denominators and
  /// persisted local configuration-change annotations.
  timelineCohort(req: CohortTimelineRequest): Promise<AnalysisResponse<CohortTimelineResult>>;
  /// Scoped deterministic anomaly explanation: resolves the optional cohort scope BEFORE
  /// detection, then returns the volume/size/efficiency decomposition of each spend spike. Both
  /// transports delegate to the same shared store orchestrator, so HTTP and Tauri return identical
  /// rows for a fixture.
  anomalyWhy(req: AnomalyWhyRequest): Promise<AnomalyWhy[]>;
  /// Offline counterfactual cost experiment: reprices the cohort's captured usage across the
  /// axis grid (model / pricing snapshot / cache strategy), optionally gated by ingested quality.
  /// Executes the grid — no re-execution, no payload read. Complements the read-only `frontier`
  /// (captured points); this is the counterfactual grid + Pareto set.
  runExperiment(req: ExperimentRequest): Promise<ExperimentResult>;

  // ---- Saved investigations: the durable, cross-transport source of truth ----
  /// All saved investigations, newest-updated first. Both transports read the SAME SQLite table, so
  /// the browser and desktop see one source of truth.
  listInvestigations(): Promise<SavedInvestigationV2[]>;
  /// Upsert one investigation (keyed on `id`). The store caps one at 256 KiB and the list at 1 MiB.
  /// The DTO's `state` omits transient focus, which is therefore never persisted.
  saveInvestigation(inv: SavedInvestigationV2): Promise<void>;
  /// Delete a saved investigation by id (idempotent).
  deleteInvestigation(id: string): Promise<void>;

  // ---- Savings action lifecycle: apply / dismiss / unaccept, per opportunity+cohort ----
  /// Mark an opportunity Applied against the exact cohort (persists the full snapshot for later
  /// verification). Rejects (409) on a cohort-hash collision or an incompatible in-place transition.
  acceptSavings(req: SavingsActionRequest): Promise<void>;
  /// Dismiss an opportunity against the exact cohort (status="dismissed").
  dismissSavings(req: SavingsActionRequest): Promise<void>;
  /// Remove the persisted action for this opportunity+cohort (unaccept).
  unacceptSavings(id: SavingsActionIdentity): Promise<void>;
  /// The persisted savings actions (applied/dismissed), each with its compatibility warnings.
  savingsActions(): Promise<SavingsAction[]>;
  /// Cohort-scoped observed-reduction verification: re-resolves the stored scope/baseline/match
  /// over equal before/after windows around the action, excluding the intervention day. Stays
  /// `verifying` until a complete after window exists; aggregate-only always warns. NOT global realization.
  verifySavings(req: SavingsVerifyRequest): Promise<SavingsVerifyResult>;
}

/// A user-authored annotation on a run. The user's own words — never provider payload.
export interface RunNote {
  run_id: string;
  tags: string[];
  note_text: string;
  starred: boolean;
  updated_at?: string;
}
