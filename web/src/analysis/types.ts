// Canonical CohortSpec DTOs — a byte-exact mirror of the serde wire form
// emitted by `tare-core/src/cohort.rs`. snake_case field names; internally-tagged unions match the
// Rust `#[serde(tag = ...)]` tags (`op` / `mode` / `kind`). Canonicalization + hashing live in
// `serialize.ts`; these are the shapes that cross the wire and get hashed.

export type CohortEntity = "run" | "step";

/// Extends RollupDim; `step` is the step-label correlation dimension (RollupDim::Step compat).
export type CohortDimension =
  | "step"
  | "component"
  | "parent"
  | "tool"
  | "agent"
  | "effort"
  | "model"
  | "provider"
  | "session"
  | "mcp_server"
  | "commit"
  | "author"
  | "template"
  | "source"
  | "workload_key"
  | "cache_class"
  | "ttl"
  | "stop_reason"
  | "run_date";

export interface StepRef {
  run_id: string;
  step_ordinal: number;
}

/// Internally tagged by `op` (matches Rust `#[serde(tag = "op")]`). AND-composed across filters;
/// `in` values are OR-composed.
export type CohortFilter =
  | { op: "eq"; dimension: CohortDimension; value: string }
  | { op: "in"; dimension: CohortDimension; values: string[] }
  | { op: "gte_micros"; value: number }
  | { op: "lte_micros"; value: number }
  | { op: "run_ids"; ids: string[] }
  | { op: "step_refs"; refs: StepRef[] }
  | { op: "tag"; value: string }
  | { op: "quality_range"; min?: number | null; max?: number | null };

/// Internally tagged by `mode`. `effective_dated` is the default.
export type PricingMode =
  | { mode: "effective_dated" }
  | { mode: "latest" }
  | { mode: "as_of"; date: string };

export type CohortMetric = "spend_micros" | "tokens" | "cache_hit_rate";

export type Normalization = "absolute" | "share_of_selection" | "per_run" | "per_outcome";

export type MeteredOutcome =
  | "pull_requests"
  | "commits"
  | "lines_added_per1k"
  | "active_hours"
  | "sessions"
  | "successful_runs";

/// Internally tagged by `kind`. The Metered inner field is `metered` (not `kind`) to avoid colliding
/// with the tag — see the tare-core cohort module note.
export type OutcomeDenominator =
  | { kind: "work_unit"; name: string }
  | { kind: "metered"; metered: MeteredOutcome };

export interface CohortSpec {
  from?: string | null;
  to?: string | null;
  timezone: string;
  entity: CohortEntity;
  filters: CohortFilter[];
  pricing: PricingMode;
  metric: CohortMetric;
  normalization: Normalization;
  outcome_denominator?: OutcomeDenominator | null;
}

// ---- Resolve result — mirrors tare_core::cohort ----

/// A resolved run or step in COHORT WIRE transport (snake_case; mirrors `tare_core::cohort`).
/// `step_ordinal` present only at Step grain. This is the wire DTO and can represent ONLY run/step
/// grain — do NOT reuse it for durable UI selection/pin/comparison state, which spans run, step,
/// session, template, and cohort. That is `UiEntityRef` in `analysis/state.ts`.
export interface CohortEntityRef {
  run_id: string;
  step_ordinal?: number;
}

/// One resolved entity's contribution. `matched_micros` is scoped spend (steps passing step-scoped
/// filters); `whole_entity_micros` is the entity's full spend. Cohort totals + sorting use matched.
export interface CohortEntitySummary {
  entity: CohortEntityRef;
  matched_micros: number;
  whole_entity_micros: number;
  matched_step_count: number;
}

export interface CohortResolveResult {
  cohort_id?: string;
  run_ids: string[];
  step_refs?: StepRef[];
  run_count: number;
  step_count: number;
  total_micros: number;
  entity_rows: CohortEntitySummary[];
}

// ---- Facet result — mirrors tare_core::cohort ----

/// One faceted value's selection-vs-baseline profile. SUPPORT is an entity count; SHARE is support /
/// non-missing entities; SPEND share is step-level micros / cohort micros — support-share and
/// spend-share are NOT substitutes. `lift_ratio` is absent when baseline support share is 0.
/// Support shares need not sum to 100% for multi-valued dimensions — do not render as an exclusive
/// stacked partition. `missing` is reported separately, never folded into a value.
export interface FacetRow {
  value: string;
  selection_support: number;
  baseline_support: number;
  selection_micros: number;
  baseline_micros: number;
  selection_support_share_pct: number;
  baseline_support_share_pct: number;
  selection_spend_share_pct: number;
  baseline_spend_share_pct: number;
  delta_support_share_points: number;
  lift_ratio?: number;
  selection_missing_pct: number;
  baseline_missing_pct: number;
}

export interface CohortFacetResult {
  dimension: CohortDimension;
  rows: FacetRow[];
}

export interface CohortFacetRequest {
  selection: CohortSpec;
  baseline: CohortSpec;
  dimension: CohortDimension;
}

// ---- Compare — mirrors tare_core::cohort ----

/// How a comparison pairs selection vs baseline. `aggregate_only` always carries a confounding
/// warning; `workload_key` is preferred when both cohorts carry it.
export type MatchRule =
  | { kind: "workload_key"; key: string }
  | { kind: "template_lineage"; hash: string }
  | { kind: "aggregate_only" };

export interface CohortCompareRequest {
  selection: CohortSpec;
  baseline: CohortSpec;
  match: MatchRule;
}

/// The three deltas reuse the deterministic anomaly arithmetic and sum EXACTLY to
/// `total_delta_micros`: volume (entity-count change), size (tokens-per-entity), efficiency (residual
/// $/token). `compatibility_warnings` flag pricing/workload/fidelity mismatches.
export interface CohortCompareResult {
  selection: CohortResolveResult;
  baseline: CohortResolveResult;
  total_delta_micros: number;
  total_delta_pct?: number;
  volume_delta_micros: number;
  size_delta_micros: number;
  efficiency_delta_micros: number;
  compatibility_warnings: string[];
}

// ---- Cross-run search — mirrors tare_core::cohort ----

/// Allow-listed search fields — there is NO payload option. `id`/`hash` match exactly or by prefix;
/// `label`/`tag`/`note`/`model`/`config` use case-insensitive substring.
export type SearchField = "id" | "label" | "hash" | "tag" | "note" | "model" | "config";

export interface CohortSearchRequest {
  cohort: CohortSpec;
  query: string;
  fields?: SearchField[];
  limit?: number;
}

/// Matching entities within the resolved cohort, capped at 200; `truncated` when more matched.
export interface CohortSearchResult {
  entities: CohortEntityRef[];
  truncated: boolean;
}

// ---- Authoritative scoped daily timeline ----

export type TimelineGroup = "total" | "provider" | "model" | "cause";

export interface CohortTimelineRequest {
  cohort: CohortSpec;
  group: TimelineGroup;
}

export type TimelineUnit =
  | "estimated_micro_usd"
  | "tokens"
  | "percent"
  | "estimated_micro_usd_per_run"
  | "tokens_per_run"
  | "estimated_micro_usd_per_outcome"
  | "tokens_per_outcome";

export interface TimelinePoint {
  day: string;
  value: number | null;
  support_count: number;
  denominator?: number;
  unavailable_reason?: string;
}

export interface TimelineSeriesResult {
  key: string;
  points: TimelinePoint[];
}

export interface TimelineConfigEvent {
  occurred_at: string;
  day: string;
  source: "settings";
  changed_fields: string[];
}

export interface CohortTimelineResult {
  days: string[];
  run_count: number;
  unit: TimelineUnit;
  series: TimelineSeriesResult[];
  config_events: TimelineConfigEvent[];
}

// ---- Analysis provenance envelope — mirrors tare_core::cohort ----

/// Attribution granularity actually available. `coarse` is the honest floor for provider-total
/// allocation; `cost_class` when only cache/output classes are known; `component` only with real
/// prompt-component attribution.
export type ComponentFidelity = "component" | "cost_class" | "coarse";

/// How spend was allocated to entities. Cohort endpoints use `provider_counts`.
export type AllocationMethod =
  | "provider_counts"
  | "byte_weight"
  | "derived"
  | "counterfactual_reprice";

/// Epistemic class of the reported values. Cohort dollars are `derived` (price × counts).
export type ValueClass = "observed" | "derived" | "allocated" | "counterfactual";

/// Which pricing edition + mode produced the money. `mode` mirrors PricingMode's tag without
/// the payload.
export interface PricingEdition {
  version: string;
  effective_date: string;
  mode: "effective" | "latest" | "as_of";
}

/// Provenance for one analysis response. Assembled by the endpoint from real
/// store + pricing state — never invented to look more confident. `coverage_status` is `unknown`
/// without a defensible denominator; `priced_token_share_pct` is present only when tokens are
/// nonzero and equals `100 - unpriced_token_share_pct`.
export interface AnalysisProvenance {
  refreshed_at: string;
  scope: CohortSpec;
  capture_sources: string[];
  coverage_status: "unknown" | "partial" | "full";
  priced_token_share_pct?: number;
  component_fidelity: ComponentFidelity;
  pricing_edition: PricingEdition;
  allocation_method: AllocationMethod;
  value_class: ValueClass;
  assumptions: string[];
}

/// The uniform analysis response envelope: the typed `data` payload plus its provenance.
export interface AnalysisResponse<T> {
  data: T;
  provenance: AnalysisProvenance;
}

// ---- Offline cost experiment — mirrors tare_core::experiment ----

/// An experiment axis (internally tagged by `kind`, values under `values`; matches Rust
/// `#[serde(tag="kind", content="values")]`). The reserved model/pricing value `"*as-captured*"`
/// keeps the run's captured config for that cell (the baseline); `"decache"` is the CacheStrategy
/// value that folds cached tokens back into fresh input.
export type Axis =
  | { kind: "model"; values: string[] }
  | { kind: "pricing_snapshot"; values: string[] }
  | { kind: "cache_strategy"; values: string[] };

/// One resolved coordinate value within a cell (internally tagged by `axis`). `model`/
/// `pricing_snapshot` carry `null` for the as-captured baseline; `cache_strategy` carries a bool
/// (`true` = decache).
export type AxisValue =
  | { axis: "model"; value: string | null }
  | { axis: "pricing_snapshot"; value: string | null }
  | { axis: "cache_strategy"; value: boolean };

/// The experiment spec: a set of axes. The only objective is minimize cost — quality is a
/// user-supplied constraint, never a computed score.
export interface CostExperiment {
  axes: Axis[];
}

/// Optional ingested-quality gate: applied to the cohort's captured runs, never to
/// counterfactual cells. Inclusive bounds; omit a side for unbounded.
export interface QualityConstraint {
  min?: number;
  max?: number;
}

/// `POST /__tare/experiment` body. `experiment` is the axis grid; `quality_constraint` gates which
/// captured runs enter the grid.
export interface ExperimentRequest {
  cohort: CohortSpec;
  experiment: CostExperiment;
  quality_constraint?: QualityConstraint;
}

/// One evaluated grid cell: a full coordinate + its repriced cost. `approximate` is true when a
/// model was swapped (the tokenizer caveat applies). `quality` is absent in this layer.
export interface ExperimentCell {
  coords: AxisValue[];
  label: string[];
  cost_micros: number;
  quality?: number;
  approximate: boolean;
}

/// The result of enumerating + repricing the whole grid (repriced from stored counts — no
/// re-execution, no payload read). `pareto` indexes the non-dominated cells in `cells`.
export interface ExperimentResult {
  cells: ExperimentCell[];
  pareto: number[];
  baseline_micros: number;
  best_micros: number;
  best_saving_micros: number;
  pricing_version: string;
  estimated: boolean;
  approximate: boolean;
}

/// One real captured run on the read-only cost×quality frontier. Quality is a user-supplied scalar;
/// its recorded source is transported when known and is never inferred for an unscored point.
export interface FrontierPoint {
  run_id: string;
  cost_micros: number;
  quality?: number;
  quality_source?: string;
  on_frontier: boolean;
}

export interface Frontier {
  points: FrontierPoint[];
  has_quality: boolean;
  pricing_version: string;
  estimated: boolean;
}

// ---- Savings action lifecycle — mirrors tare_core::savings ----

/// Baseline rule persisted with a savings action. snake_case wire form of the UI BaselineSpec.
export interface BaselineDto {
  kind: "prior_window" | "rest_of_scope" | "pinned_run" | "explicit_cohort";
  label: string;
  cohort: CohortSpec;
  sample_count?: number;
}

/// Apply/dismiss request: the exact intervention to persist + verify later. `match` uses the
/// shared MatchRule union.
export interface SavingsActionRequest {
  opportunity_key: string;
  cohort: CohortSpec;
  baseline?: BaselineDto;
  match: MatchRule;
  metric: CohortMetric;
  normalization: Normalization;
  outcome_denominator?: OutcomeDenominator;
  expected_low_micros?: number;
  expected_point_micros?: number;
  expected_high_micros?: number;
  quality_guardrail?: number;
}

/// Identity of a persisted action: opportunity + the cohort hash it was taken against. Unaccept
/// removes the exact row by this.
export interface SavingsActionIdentity {
  opportunity_key: string;
  cohort_hash: string;
}

/// A stored savings action: the persisted row + computed `compatibility_warnings`. `status` is
/// `applied` | `dismissed` (the only persisted statuses); verification states are derived.
export interface SavingsAction {
  opportunity_key: string;
  cohort_hash: string;
  status: string;
  acted_at: string;
  cohort: CohortSpec;
  baseline?: BaselineDto;
  match: MatchRule;
  metric: CohortMetric;
  normalization: Normalization;
  outcome_denominator?: OutcomeDenominator;
  expected_low_micros?: number;
  expected_point_micros?: number;
  expected_high_micros?: number;
  quality_guardrail?: number;
  compatibility_warnings: string[];
}

/// A verification request: action identity + optional as-of date and window size. Verification
/// re-resolves the stored cohort/baseline/match over equal before/after windows around `acted_at`.
export interface SavingsVerifyRequest {
  opportunity_key: string;
  cohort_hash: string;
  as_of_date?: string;
  window_days?: number;
}

/// Cohort-scoped OBSERVED-REDUCTION result — never the global realization. `status` is
/// `verifying` (incomplete after window) | `observed_reduction` (complete + positive) | `not_observed`.
/// With a baseline, `observed_reduction_micros` is the difference-in-differences adjusted reduction;
/// without one it is the unadjusted selection reduction and carries a warning.
export interface SavingsVerifyResult {
  status: "verifying" | "observed_reduction" | "not_observed";
  complete: boolean;
  selection_before_micros: number;
  selection_after_micros: number;
  baseline_before_micros?: number;
  baseline_after_micros?: number;
  observed_reduction_micros: number;
  matched_before: number;
  matched_after: number;
  unmatched_before: number;
  unmatched_after: number;
  /// Present with a stored baseline so the UI can disclose every exclusion from the shared
  /// workload/template intersection rather than reporting selection coverage alone.
  baseline_matched_before?: number;
  baseline_matched_after?: number;
  baseline_unmatched_before?: number;
  baseline_unmatched_after?: number;
  compatibility_warnings: string[];
}
