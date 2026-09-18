//! Canonical `CohortSpec`: the scope/selection contract shared by every
//! Calibrated Bench analysis endpoint. This module defines the types + normative serde wire form,
//! a **canonical JSON** serialization, and a stable **cohort hash** — all mirrored byte-for-byte by
//! `web/src/analysis/{types,serialize}.ts` so a scope hashes identically in Rust and TypeScript.
//!
//! Canonicalization: object keys sorted; `In.values` and `RunIds.ids` sorted + deduped;
//! `StepRefs.refs` sorted by `(run_id, step_ordinal)`; filters sorted by `(op, dimension, canonical
//! value)` and deduplicated; every field serialized explicitly (Options as `null`) so equivalent scopes produce
//! identical bytes. The hash is `{fnv1a_64(canonical_json):016x}` reusing [`crate::canon`].
//!
//! Contract note: `OutcomeDenominator::Metered { kind: MeteredOutcome }` collides with the `kind`
//! internal tag. The inner field therefore serializes as `metered`
//! (`{"kind":"metered","metered":"pull_requests"}`).
//! Timezones are resolved against the bundled IANA database, so validation and DST bucketing stay
//! offline and deterministic across platforms.

use serde::{Deserialize, Serialize};

/// Max filters in one spec.
pub const MAX_FILTERS: usize = 32;
/// Max values in one `In` filter.
pub const MAX_IN_VALUES: usize = 100;
/// Max explicit run ids in one `RunIds` filter.
pub const MAX_RUN_IDS: usize = 5_000;
/// Max explicit step refs in one `StepRefs` filter.
pub const MAX_STEP_REFS: usize = 10_000;
/// Maximum size of a user-supplied dimension/id/denominator label in a cohort spec.
pub const MAX_COHORT_LABEL_CHARS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSpec {
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    pub timezone: String,
    pub entity: CohortEntity,
    #[serde(default)]
    pub filters: Vec<CohortFilter>,
    pub pricing: PricingMode,
    pub metric: CohortMetric,
    pub normalization: Normalization,
    #[serde(default)]
    pub outcome_denominator: Option<OutcomeDenominator>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortEntity {
    Run,
    Step,
}

/// The scope/facet dimension. Extends [`crate::rollup::RollupDim`]; `StepLabel` serializes as
/// `"step"` for compatibility with `RollupDim::Step` (it means the step-label correlation dimension,
/// not a step ordinal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortDimension {
    #[serde(rename = "step")]
    StepLabel,
    Component,
    Parent,
    Tool,
    Agent,
    Effort,
    Model,
    Provider,
    Session,
    McpServer,
    Commit,
    Author,
    Template,
    Source,
    WorkloadKey,
    CacheClass,
    Ttl,
    StopReason,
    RunDate,
}

impl CohortDimension {
    /// The wire token (matches the serde `rename_all = "snake_case"` output, with `StepLabel` ->
    /// `"step"`). Reused for canonical filter ordering and for parity with `RollupDim::as_str`.
    pub fn as_str(self) -> &'static str {
        match self {
            CohortDimension::StepLabel => "step",
            CohortDimension::Component => "component",
            CohortDimension::Parent => "parent",
            CohortDimension::Tool => "tool",
            CohortDimension::Agent => "agent",
            CohortDimension::Effort => "effort",
            CohortDimension::Model => "model",
            CohortDimension::Provider => "provider",
            CohortDimension::Session => "session",
            CohortDimension::McpServer => "mcp_server",
            CohortDimension::Commit => "commit",
            CohortDimension::Author => "author",
            CohortDimension::Template => "template",
            CohortDimension::Source => "source",
            CohortDimension::WorkloadKey => "workload_key",
            CohortDimension::CacheClass => "cache_class",
            CohortDimension::Ttl => "ttl",
            CohortDimension::StopReason => "stop_reason",
            CohortDimension::RunDate => "run_date",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRef {
    pub run_id: String,
    pub step_ordinal: u32,
}

/// A resolved run or step. `step_ordinal` is present only at Step grain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityRef {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_ordinal: Option<u32>,
}

/// One resolved entity's contribution: `matched_micros` is the scoped spend from steps that pass
/// step-scoped filters; `whole_entity_micros` is the entity's full spend. A run row after
/// step-scoped filtering exposes both; cohort totals and sorting use the matched/scoped figure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortEntitySummary {
    pub entity: EntityRef,
    pub matched_micros: i64,
    pub whole_entity_micros: i64,
    pub matched_step_count: u32,
}

/// The resolved cohort. `total_micros` is the scoped total (Σ matched_micros). Deterministic:
/// `run_ids` sorted; `step_refs` (Step grain only) sorted by (run_id, step_ordinal).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortResolveResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cohort_id: Option<String>,
    pub run_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_refs: Option<Vec<StepRef>>,
    pub run_count: u32,
    pub step_count: u32,
    pub total_micros: i64,
    pub entity_rows: Vec<CohortEntitySummary>,
}

/// One faceted value's selection-vs-baseline profile. SUPPORT is an
/// entity count (a run/step "carries" the value); SHARE is support / non-missing entities; SPEND
/// share is step-level micros / cohort micros. Support-share and spend-share are NOT substitutes.
/// Carries `f64` ratios so it cannot derive `Eq` (unlike the hashed spec types).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FacetRow {
    pub value: String,
    pub selection_support: i64,
    pub baseline_support: i64,
    pub selection_micros: i64,
    pub baseline_micros: i64,
    pub selection_support_share_pct: f64,
    pub baseline_support_share_pct: f64,
    pub selection_spend_share_pct: f64,
    pub baseline_spend_share_pct: f64,
    pub delta_support_share_points: f64,
    /// `selection_support_share / baseline_support_share`. Omitted when baseline support share is 0
    /// (use `delta_support_share_points` instead). A large lift is an association, not causation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_ratio: Option<f64>,
    /// Missing rate is reported separately and never folded into an `unlabeled` value. Cohort-level,
    /// so it repeats across every row of the same facet.
    pub selection_missing_pct: f64,
    pub baseline_missing_pct: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CohortFacetResult {
    pub dimension: CohortDimension,
    pub rows: Vec<FacetRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortFacetRequest {
    pub selection: CohortSpec,
    pub baseline: CohortSpec,
    pub dimension: CohortDimension,
}

/// How a comparison pairs a selection against a baseline. `workload_key` is the preferred
/// rule when both cohorts carry it; `template_lineage` matches on the stable `system_hash`;
/// `aggregate_only` compares the two totals and ALWAYS carries a confounding warning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatchRule {
    WorkloadKey { key: String },
    TemplateLineage { hash: String },
    AggregateOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortCompareRequest {
    pub selection: CohortSpec,
    pub baseline: CohortSpec,
    // Wire field is `match` (a Rust keyword), so the field is `match_rule` with an explicit rename.
    #[serde(rename = "match")]
    pub match_rule: MatchRule,
}

/// The allow-listed fields cross-run search may read. There is NO payload option by
/// construction — search never touches prompt/response text. `id`/`hash` match
/// exactly or by prefix; `label`/`tag`/`note`/`model`/`config` use case-insensitive substring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    Id,
    Label,
    Hash,
    Tag,
    Note,
    Model,
    Config,
}

impl SearchField {
    /// The default field set (all of them) when a request omits `fields`.
    pub const ALL: [SearchField; 7] = [
        SearchField::Id,
        SearchField::Label,
        SearchField::Hash,
        SearchField::Tag,
        SearchField::Note,
        SearchField::Model,
        SearchField::Config,
    ];
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSearchRequest {
    pub cohort: CohortSpec,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<SearchField>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Cross-run search result: matching entities within the resolved cohort, capped at 200.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortSearchResult {
    pub entities: Vec<EntityRef>,
    pub truncated: bool,
}

/// The authoritative scoped timeline grouping. `Cause` is computed from the
/// counts-only attribution report over the already-resolved steps; it is not a captured dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineGroup {
    Total,
    Provider,
    Model,
    Cause,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CohortTimelineRequest {
    pub cohort: CohortSpec,
    pub group: TimelineGroup,
}

/// Unit of every non-null point in a timeline response. Dollar units say `estimated` explicitly;
/// token values remain observed provider counts, and ratios are percentages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineUnit {
    EstimatedMicroUsd,
    Tokens,
    Percent,
    EstimatedMicroUsdPerRun,
    TokensPerRun,
    EstimatedMicroUsdPerOutcome,
    TokensPerOutcome,
}

/// One dense calendar-day value. `value=null` is never coerced to zero: the point carries the
/// reason its requested denominator or attribution is unavailable. A real zero with a positive
/// denominator remains `Some(0.0)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelinePoint {
    pub day: String,
    pub value: Option<f64>,
    pub support_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelineSeries {
    pub key: String,
    pub points: Vec<TimelinePoint>,
}

/// A real local Settings save, persisted as metadata only. `changed_fields` contains sorted field
/// paths, never configuration values, secrets, URLs, payloads, or prompt/response text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineConfigEvent {
    pub occurred_at: String,
    pub day: String,
    pub source: String,
    pub changed_fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CohortTimelineResult {
    pub days: Vec<String>,
    pub run_count: u32,
    pub unit: TimelineUnit,
    pub series: Vec<TimelineSeries>,
    pub config_events: Vec<TimelineConfigEvent>,
}

/// Hard cap on search results.
pub const SEARCH_RESULT_CAP: usize = 200;

// ---- Analysis provenance envelope ----
//
// Every Calibrated Bench analysis response is wrapped in [`AnalysisResponse`] so the caller sees
// *how* a number was produced, not just the number. The envelope is NOT retrofitted onto legacy
// endpoints. `allocation_method` / `value_class` are assigned by the endpoint that
// computes the payload — never guessed by the client. The pricing edition, mode, and the
// honest `coverage_status`/`assumptions` are filled from real store + pricing state; nothing here
// is invented to look more confident than the underlying capture allows.

/// Attribution granularity actually available for the payload. `Coarse` is the honest floor
/// for out-of-band provider-total allocation; `CostClass` when only cache/output classes are known;
/// `Component` only when prompt-component attribution exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentFidelity {
    Component,
    CostClass,
    Coarse,
}

/// How spend was allocated to entities. `ProviderCounts` = provider-reported token counts
/// priced by the table (the resolve/facet/compare/search path); the others are reserved for future
/// byte-weighted or counterfactual endpoints and are never emitted speculatively.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationMethod {
    ProviderCounts,
    ByteWeight,
    Derived,
    CounterfactualReprice,
}

/// Epistemic class of the reported values. Cohort dollar figures are `Derived` (price ×
/// observed counts); `Observed` is reserved for directly measured quantities, `Allocated` for
/// split totals, `Counterfactual` for repriced "what-if" figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueClass {
    Observed,
    Derived,
    Allocated,
    Counterfactual,
}

/// Which pricing edition + mode produced the money. `version`/`effective_date` come straight
/// from the [`crate::pricing::PricingTable`]; `mode` mirrors the request's [`PricingMode`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingEdition {
    pub version: String,
    pub effective_date: String,
    /// `effective` | `latest` | `as_of` — the wire tags of [`PricingMode`] without the payload.
    pub mode: String,
}

impl PricingEdition {
    /// Derive the `mode` string from a request's [`PricingMode`] (drops the `as_of` date payload —
    /// the date already lives in `effective_date` for that mode).
    pub fn mode_of(pricing: &PricingMode) -> &'static str {
        match pricing {
            PricingMode::EffectiveDated => "effective",
            PricingMode::Latest => "latest",
            PricingMode::AsOf { .. } => "as_of",
        }
    }
}

/// Provenance for one analysis response. Assembled by the endpoint from real
/// store + pricing state — `coverage_status` is `unknown` without a defensible denominator,
/// `priced_token_share_pct` is present only when tokens are nonzero and equals
/// `100 - unpriced_token_share_pct`, `capture_sources` is the distinct capture sources
/// actually present, and `assumptions` records every honest caveat (e.g. legacy date bucketing).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisProvenance {
    /// RFC3339 instant the data was computed (stamped by the clock-owning transport edge).
    pub refreshed_at: String,
    /// The exact scope this response answers — echoed so the caller can hash/pin it.
    pub scope: CohortSpec,
    /// Distinct capture sources present in the store (sorted).
    pub capture_sources: Vec<String>,
    /// Coverage vs a denominator: `unknown` out-of-band (no defensible denominator) — never faked.
    pub coverage_status: crate::confidence::CoverageStatus,
    /// Present only when total tokens are nonzero; `100 - unpriced_token_share_pct`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priced_token_share_pct: Option<i64>,
    pub component_fidelity: ComponentFidelity,
    pub pricing_edition: PricingEdition,
    pub allocation_method: AllocationMethod,
    pub value_class: ValueClass,
    /// Honest caveats that shaped the numbers (e.g. `legacy_date_bucket`).
    pub assumptions: Vec<String>,
}

/// The uniform analysis response envelope: the typed `data` payload plus its provenance.
/// Generic over the payload so resolve/facet/compare/search all share one wire shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisResponse<T> {
    pub data: T,
    pub provenance: AnalysisProvenance,
}

/// Selection-vs-baseline comparison. The three deltas reuse the deterministic anomaly
/// arithmetic and sum EXACTLY to `total_delta_micros`: volume = entity-count change at baseline
/// avg size/mix; size = tokens-per-entity change at baseline $/token; efficiency = the residual
/// ($/token — caching/model mix). `f64` percent → `PartialEq`, no `Eq`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CohortCompareResult {
    pub selection: CohortResolveResult,
    pub baseline: CohortResolveResult,
    pub total_delta_micros: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_delta_pct: Option<f64>,
    pub volume_delta_micros: i64,
    pub size_delta_micros: i64,
    pub efficiency_delta_micros: i64,
    pub compatibility_warnings: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CohortFilter {
    Eq {
        dimension: CohortDimension,
        value: String,
    },
    In {
        dimension: CohortDimension,
        values: Vec<String>,
    },
    GteMicros {
        value: i64,
    },
    LteMicros {
        value: i64,
    },
    RunIds {
        ids: Vec<String>,
    },
    StepRefs {
        refs: Vec<StepRef>,
    },
    Tag {
        value: String,
    },
    QualityRange {
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PricingMode {
    #[default]
    EffectiveDated,
    Latest,
    AsOf {
        date: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortMetric {
    SpendMicros,
    Tokens,
    CacheHitRate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Normalization {
    Absolute,
    ShareOfSelection,
    PerRun,
    PerOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeDenominator {
    WorkUnit {
        name: String,
    },
    Metered {
        // Serialized as `metered` to avoid colliding with the `kind` internal tag (see module doc).
        #[serde(rename = "metered")]
        kind: MeteredOutcome,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeteredOutcome {
    PullRequests,
    Commits,
    LinesAddedPer1k,
    ActiveHours,
    Sessions,
    SuccessfulRuns,
}

/// A validation failure with a stable, specific message. Typed so tests and the HTTP layer
/// can match exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CohortError {
    EmptyTimezone,
    InvalidTimezone(String),
    TooManyFilters(usize),
    TooManyInValues(usize),
    TooManyRunIds(usize),
    TooManyStepRefs(usize),
    InvalidDate(&'static str, String),
    InvalidDateRange,
    InvalidLabel(&'static str),
    InvalidQualityRange,
    EmptyWorkUnitName,
    PerOutcomeMissingDenominator,
    UnexpectedOutcomeDenominator,
    /// (metric, normalization) — the disallowed pairing.
    InvalidNormalizationForMetric(&'static str, &'static str),
}

impl std::fmt::Display for CohortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CohortError::EmptyTimezone => write!(f, "timezone is empty"),
            CohortError::InvalidTimezone(tz) => write!(f, "invalid IANA timezone: {tz:?}"),
            CohortError::TooManyFilters(n) => {
                write!(f, "too many filters: {n} (max {MAX_FILTERS})")
            }
            CohortError::TooManyInValues(n) => {
                write!(
                    f,
                    "too many values in an In filter: {n} (max {MAX_IN_VALUES})"
                )
            }
            CohortError::TooManyRunIds(n) => {
                write!(f, "too many run ids: {n} (max {MAX_RUN_IDS})")
            }
            CohortError::TooManyStepRefs(n) => {
                write!(f, "too many step refs: {n} (max {MAX_STEP_REFS})")
            }
            CohortError::InvalidDate(field, value) => {
                write!(f, "invalid {field} date {value:?}; expected YYYY-MM-DD")
            }
            CohortError::InvalidDateRange => write!(f, "cohort from date must not exceed to date"),
            CohortError::InvalidLabel(field) => write!(
                f,
                "invalid {field}: expected a non-blank, control-free label of at most {MAX_COHORT_LABEL_CHARS} characters"
            ),
            CohortError::InvalidQualityRange => {
                write!(f, "quality range min must not exceed max")
            }
            CohortError::EmptyWorkUnitName => write!(f, "work-unit denominator name is empty"),
            CohortError::PerOutcomeMissingDenominator => {
                write!(
                    f,
                    "per_outcome normalization requires an outcome_denominator"
                )
            }
            CohortError::UnexpectedOutcomeDenominator => write!(
                f,
                "outcome_denominator is only valid with per_outcome normalization"
            ),
            CohortError::InvalidNormalizationForMetric(m, n) => {
                write!(f, "normalization {n} is invalid for metric {m}")
            }
        }
    }
}

impl std::error::Error for CohortError {}

/// Authoritative IANA timezone validation. Resolves the identifier against
/// jiff's bundled IANA database, so well-formed-but-nonexistent zones (e.g. `Not/AZone`) are
/// rejected, not just malformed strings. `UTC` is always available. Offline + deterministic across
/// macOS/Windows/Linux because the tzdb is compiled in (`tzdb-bundle-always`).
fn validate_timezone(tz: &str) -> Result<(), CohortError> {
    if tz.is_empty() {
        return Err(CohortError::EmptyTimezone);
    }
    if crate::tz::is_valid_zone(tz) {
        Ok(())
    } else {
        Err(CohortError::InvalidTimezone(tz.to_string()))
    }
}

fn validate_date(field: &'static str, value: &str) -> Result<i64, CohortError> {
    crate::calendar::parse_date(value)
        .filter(|days| crate::calendar::format_date(*days) == value)
        .ok_or_else(|| CohortError::InvalidDate(field, value.to_string()))
}

fn validate_label(field: &'static str, value: &str) -> Result<(), CohortError> {
    if value.trim().is_empty()
        || value.chars().count() > MAX_COHORT_LABEL_CHARS
        || value.chars().any(char::is_control)
    {
        Err(CohortError::InvalidLabel(field))
    } else {
        Ok(())
    }
}

impl CohortSpec {
    /// Validate the config-independent contract: timezone format, filter/list limits, and the
    /// metric×normalization matrix. NOTE: `WorkUnit.name` resolution against configured `[unit]`
    /// entries is a resolve-time check since it needs the loaded config; here we only
    /// require the name be non-empty. The shared response layer enforces the 256 KiB body cap.
    pub fn validate(&self) -> Result<(), CohortError> {
        validate_timezone(&self.timezone)?;
        let from = self
            .from
            .as_deref()
            .map(|date| validate_date("from", date))
            .transpose()?;
        let to = self
            .to
            .as_deref()
            .map(|date| validate_date("to", date))
            .transpose()?;
        if from.zip(to).is_some_and(|(from, to)| from > to) {
            return Err(CohortError::InvalidDateRange);
        }
        if let PricingMode::AsOf { date } = &self.pricing {
            validate_date("pricing as_of", date)?;
        }

        if self.filters.len() > MAX_FILTERS {
            return Err(CohortError::TooManyFilters(self.filters.len()));
        }
        for filter in &self.filters {
            match filter {
                CohortFilter::In { values, .. } if values.len() > MAX_IN_VALUES => {
                    return Err(CohortError::TooManyInValues(values.len()));
                }
                CohortFilter::RunIds { ids } if ids.len() > MAX_RUN_IDS => {
                    return Err(CohortError::TooManyRunIds(ids.len()));
                }
                CohortFilter::StepRefs { refs } if refs.len() > MAX_STEP_REFS => {
                    return Err(CohortError::TooManyStepRefs(refs.len()));
                }
                _ => {}
            }
            match filter {
                CohortFilter::Eq { value, .. } | CohortFilter::Tag { value } => {
                    validate_label("filter value", value)?;
                }
                CohortFilter::In { values, .. } => {
                    for value in values {
                        validate_label("In filter value", value)?;
                    }
                }
                CohortFilter::RunIds { ids } => {
                    for id in ids {
                        validate_label("run id", id)?;
                    }
                }
                CohortFilter::StepRefs { refs } => {
                    for reference in refs {
                        validate_label("step-ref run id", &reference.run_id)?;
                    }
                }
                CohortFilter::QualityRange { min, max }
                    if (*min).zip(*max).is_some_and(|(min, max)| min > max) =>
                {
                    return Err(CohortError::InvalidQualityRange);
                }
                _ => {}
            }
        }

        // Metric × normalization matrix.
        match (self.metric, self.normalization) {
            // CacheHitRate is already a ratio: only Absolute is meaningful.
            (CohortMetric::CacheHitRate, Normalization::Absolute) => {}
            (CohortMetric::CacheHitRate, other) => {
                return Err(CohortError::InvalidNormalizationForMetric(
                    "cache_hit_rate",
                    normalization_str(other),
                ));
            }
            // ShareOfSelection only makes sense for additive metrics.
            (CohortMetric::SpendMicros | CohortMetric::Tokens, _) => {}
        }
        // PerOutcome requires a denominator (quality is never a denominator).
        if self.normalization == Normalization::PerOutcome && self.outcome_denominator.is_none() {
            return Err(CohortError::PerOutcomeMissingDenominator);
        }
        if self.normalization != Normalization::PerOutcome && self.outcome_denominator.is_some() {
            return Err(CohortError::UnexpectedOutcomeDenominator);
        }
        if let Some(OutcomeDenominator::WorkUnit { name }) = &self.outcome_denominator {
            if name.trim().is_empty() {
                return Err(CohortError::EmptyWorkUnitName);
            }
            validate_label("work-unit denominator name", name)?;
        }
        Ok(())
    }

    /// Canonical JSON string: fully-explicit fields, normalized arrays, sorted filters,
    /// sorted keys, no whitespace. Byte-identical to the TypeScript `canonicalCohortJson`.
    pub fn canonical_json(&self) -> String {
        let value = self.to_canonical_value();
        serde_json::to_string(&crate::canon::canonicalize(&value)).unwrap_or_default()
    }

    /// Stable cohort hash: 16-char lowercase hex `fnv1a_64` of the canonical JSON (matches the
    /// repo's `opportunity_key`/`cohort_hash` convention). Byte-identical across Rust and TS.
    pub fn cohort_hash(&self) -> String {
        format!(
            "{:016x}",
            crate::canon::fnv1a_64(self.canonical_json().as_bytes())
        )
    }

    fn to_canonical_value(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let mut filters: Vec<(String, String, Value)> = self
            .filters
            .iter()
            .map(|filter| {
                let value = filter_to_value(filter);
                let canonical =
                    serde_json::to_string(&crate::canon::canonicalize(&value)).unwrap_or_default();
                (filter_sort_key(filter), canonical, value)
            })
            .collect();
        filters.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        filters.dedup_by(|a, b| a.2 == b.2);
        let filters: Vec<Value> = filters.into_iter().map(|(_, _, value)| value).collect();

        json!({
            "from": self.from,
            "to": self.to,
            "timezone": self.timezone,
            "entity": entity_str(self.entity),
            "filters": filters,
            "pricing": pricing_to_value(&self.pricing),
            "metric": metric_str(self.metric),
            "normalization": normalization_str(self.normalization),
            "outcome_denominator": self.outcome_denominator.as_ref().map(outcome_to_value),
        })
    }
}

fn entity_str(e: CohortEntity) -> &'static str {
    match e {
        CohortEntity::Run => "run",
        CohortEntity::Step => "step",
    }
}

fn metric_str(m: CohortMetric) -> &'static str {
    match m {
        CohortMetric::SpendMicros => "spend_micros",
        CohortMetric::Tokens => "tokens",
        CohortMetric::CacheHitRate => "cache_hit_rate",
    }
}

fn normalization_str(n: Normalization) -> &'static str {
    match n {
        Normalization::Absolute => "absolute",
        Normalization::ShareOfSelection => "share_of_selection",
        Normalization::PerRun => "per_run",
        Normalization::PerOutcome => "per_outcome",
    }
}

fn metered_str(k: MeteredOutcome) -> &'static str {
    match k {
        MeteredOutcome::PullRequests => "pull_requests",
        MeteredOutcome::Commits => "commits",
        MeteredOutcome::LinesAddedPer1k => "lines_added_per1k",
        MeteredOutcome::ActiveHours => "active_hours",
        MeteredOutcome::Sessions => "sessions",
        MeteredOutcome::SuccessfulRuns => "successful_runs",
    }
}

fn pricing_to_value(p: &PricingMode) -> serde_json::Value {
    use serde_json::json;
    match p {
        PricingMode::EffectiveDated => json!({ "mode": "effective_dated" }),
        PricingMode::Latest => json!({ "mode": "latest" }),
        PricingMode::AsOf { date } => json!({ "mode": "as_of", "date": date }),
    }
}

fn outcome_to_value(o: &OutcomeDenominator) -> serde_json::Value {
    use serde_json::json;
    match o {
        OutcomeDenominator::WorkUnit { name } => json!({ "kind": "work_unit", "name": name }),
        OutcomeDenominator::Metered { kind } => {
            json!({ "kind": "metered", "metered": metered_str(*kind) })
        }
    }
}

fn filter_to_value(f: &CohortFilter) -> serde_json::Value {
    use serde_json::json;
    match f {
        CohortFilter::Eq { dimension, value } => {
            json!({ "op": "eq", "dimension": dimension.as_str(), "value": value })
        }
        CohortFilter::In { dimension, values } => {
            let mut v = values.clone();
            v.sort();
            v.dedup();
            json!({ "op": "in", "dimension": dimension.as_str(), "values": v })
        }
        CohortFilter::GteMicros { value } => json!({ "op": "gte_micros", "value": value }),
        CohortFilter::LteMicros { value } => json!({ "op": "lte_micros", "value": value }),
        CohortFilter::RunIds { ids } => {
            let mut v = ids.clone();
            v.sort();
            v.dedup();
            json!({ "op": "run_ids", "ids": v })
        }
        CohortFilter::StepRefs { refs } => {
            let mut v = refs.clone();
            v.sort_by(|a, b| {
                a.run_id
                    .cmp(&b.run_id)
                    .then(a.step_ordinal.cmp(&b.step_ordinal))
            });
            v.dedup();
            let refs: Vec<_> = v
                .into_iter()
                .map(|r| json!({ "run_id": r.run_id, "step_ordinal": r.step_ordinal }))
                .collect();
            json!({ "op": "step_refs", "refs": refs })
        }
        CohortFilter::Tag { value } => json!({ "op": "tag", "value": value }),
        CohortFilter::QualityRange { min, max } => {
            json!({ "op": "quality_range", "min": min, "max": max })
        }
    }
}

/// Deterministic filter sort key `(op, dimension, canonical value)`, byte-identical to the TS
/// side. Components are joined with `\u{1f}` and multi-values with `\u{1e}` so distinct filters can
/// never collide into the same key.
fn filter_sort_key(f: &CohortFilter) -> String {
    let us = '\u{1f}';
    let rs = '\u{1e}';
    match f {
        CohortFilter::Eq { dimension, value } => {
            format!("eq{us}{}{us}{value}", dimension.as_str())
        }
        CohortFilter::In { dimension, values } => {
            let mut v = values.clone();
            v.sort();
            v.dedup();
            format!(
                "in{us}{}{us}{}",
                dimension.as_str(),
                v.join(&rs.to_string())
            )
        }
        CohortFilter::GteMicros { value } => format!("gte_micros{us}{us}{value}"),
        CohortFilter::LteMicros { value } => format!("lte_micros{us}{us}{value}"),
        CohortFilter::RunIds { ids } => {
            let mut v = ids.clone();
            v.sort();
            v.dedup();
            format!("run_ids{us}{us}{}", v.join(&rs.to_string()))
        }
        CohortFilter::StepRefs { refs } => {
            let mut v = refs.clone();
            v.sort_by(|a, b| {
                a.run_id
                    .cmp(&b.run_id)
                    .then(a.step_ordinal.cmp(&b.step_ordinal))
            });
            v.dedup();
            let joined = v
                .iter()
                .map(|r| format!("{}:{}", r.run_id, r.step_ordinal))
                .collect::<Vec<_>>()
                .join(&rs.to_string());
            format!("step_refs{us}{us}{joined}")
        }
        CohortFilter::Tag { value } => format!("tag{us}{us}{value}"),
        CohortFilter::QualityRange { min, max } => {
            let m = min.map(|n| n.to_string()).unwrap_or_default();
            let x = max.map(|n| n.to_string()).unwrap_or_default();
            format!("quality_range{us}{us}{m}{rs}{x}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(entity: CohortEntity) -> CohortSpec {
        CohortSpec {
            from: Some("2026-05-01".into()),
            to: Some("2026-06-01".into()),
            timezone: "America/Los_Angeles".into(),
            entity,
            filters: vec![],
            pricing: PricingMode::EffectiveDated,
            metric: CohortMetric::SpendMicros,
            normalization: Normalization::Absolute,
            outcome_denominator: None,
        }
    }

    #[test]
    fn serde_wire_tags_are_normative() {
        let f = CohortFilter::Eq {
            dimension: CohortDimension::Model,
            value: "claude-opus-4-8".into(),
        };
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"op":"eq","dimension":"model","value":"claude-opus-4-8"}"#
        );
        // StepLabel serializes as "step" for RollupDim compatibility.
        let sl = CohortFilter::Eq {
            dimension: CohortDimension::StepLabel,
            value: "x".into(),
        };
        assert!(serde_json::to_string(&sl)
            .unwrap()
            .contains(r#""dimension":"step""#));
        // PricingMode tagged by `mode`; OutcomeDenominator by `kind` with Metered's inner as `metered`.
        assert_eq!(
            serde_json::to_string(&PricingMode::AsOf {
                date: "2026-06-01".into()
            })
            .unwrap(),
            r#"{"mode":"as_of","date":"2026-06-01"}"#
        );
        assert_eq!(
            serde_json::to_string(&OutcomeDenominator::Metered {
                kind: MeteredOutcome::PullRequests
            })
            .unwrap(),
            r#"{"kind":"metered","metered":"pull_requests"}"#
        );
    }

    #[test]
    fn canonical_hash_is_order_and_dup_invariant() {
        // Two specs differing only in filter order and In-value order/dups hash identically.
        let mut a = base(CohortEntity::Run);
        a.filters = vec![
            CohortFilter::In {
                dimension: CohortDimension::Model,
                values: vec!["b".into(), "a".into(), "a".into()],
            },
            CohortFilter::Eq {
                dimension: CohortDimension::Provider,
                value: "anthropic".into(),
            },
            CohortFilter::Eq {
                dimension: CohortDimension::Provider,
                value: "anthropic".into(),
            },
        ];
        let mut b = base(CohortEntity::Run);
        b.filters = vec![
            CohortFilter::Eq {
                dimension: CohortDimension::Provider,
                value: "anthropic".into(),
            },
            CohortFilter::In {
                dimension: CohortDimension::Model,
                values: vec!["a".into(), "b".into()],
            },
        ];
        assert_eq!(a.canonical_json(), b.canonical_json());
        assert_eq!(a.cohort_hash(), b.cohort_hash());
        // A different value changes the hash.
        let mut c = b.clone();
        c.filters.push(CohortFilter::Tag {
            value: "anomaly".into(),
        });
        assert_ne!(b.cohort_hash(), c.cohort_hash());
    }

    #[test]
    fn steprefs_sorted_and_deduped_in_canonical_form() {
        let mut a = base(CohortEntity::Step);
        a.filters = vec![CohortFilter::StepRefs {
            refs: vec![
                StepRef {
                    run_id: "r2".into(),
                    step_ordinal: 1,
                },
                StepRef {
                    run_id: "r1".into(),
                    step_ordinal: 2,
                },
                StepRef {
                    run_id: "r1".into(),
                    step_ordinal: 2,
                },
            ],
        }];
        let json = a.canonical_json();
        // r1 before r2; the duplicate is dropped.
        assert!(
            json.contains(
                r#""refs":[{"run_id":"r1","step_ordinal":2},{"run_id":"r2","step_ordinal":1}]"#
            ),
            "{json}"
        );
    }

    #[test]
    fn timezone_validation() {
        assert!(base(CohortEntity::Run).validate().is_ok());
        let mut empty = base(CohortEntity::Run);
        empty.timezone = "".into();
        assert_eq!(empty.validate(), Err(CohortError::EmptyTimezone));
        let mut bad = base(CohortEntity::Run);
        bad.timezone = "not a zone".into();
        assert!(matches!(
            bad.validate(),
            Err(CohortError::InvalidTimezone(_))
        ));
        let mut utc = base(CohortEntity::Run);
        utc.timezone = "UTC".into();
        assert!(utc.validate().is_ok());
    }

    #[test]
    fn dates_ranges_and_labels_are_validated() {
        let mut malformed = base(CohortEntity::Run);
        malformed.from = Some("2026-2-03".into());
        assert!(matches!(
            malformed.validate(),
            Err(CohortError::InvalidDate("from", _))
        ));

        let mut reversed = base(CohortEntity::Run);
        reversed.from = Some("2026-06-02".into());
        reversed.to = Some("2026-06-01".into());
        assert_eq!(reversed.validate(), Err(CohortError::InvalidDateRange));

        let mut bad_snapshot = base(CohortEntity::Run);
        bad_snapshot.pricing = PricingMode::AsOf {
            date: "not-a-date".into(),
        };
        assert!(matches!(
            bad_snapshot.validate(),
            Err(CohortError::InvalidDate("pricing as_of", _))
        ));

        let mut bad_label = base(CohortEntity::Run);
        bad_label.filters = vec![CohortFilter::Tag {
            value: "line\nbreak".into(),
        }];
        assert_eq!(
            bad_label.validate(),
            Err(CohortError::InvalidLabel("filter value"))
        );

        let mut quality = base(CohortEntity::Run);
        quality.filters = vec![CohortFilter::QualityRange {
            min: Some(10),
            max: Some(9),
        }];
        assert_eq!(quality.validate(), Err(CohortError::InvalidQualityRange));
    }

    #[test]
    fn limits_are_enforced() {
        let mut f = base(CohortEntity::Run);
        f.filters = (0..MAX_FILTERS + 1)
            .map(|_| CohortFilter::Tag { value: "t".into() })
            .collect();
        assert!(matches!(f.validate(), Err(CohortError::TooManyFilters(_))));

        let mut inv = base(CohortEntity::Run);
        inv.filters = vec![CohortFilter::In {
            dimension: CohortDimension::Model,
            values: (0..MAX_IN_VALUES + 1).map(|i| i.to_string()).collect(),
        }];
        assert!(matches!(
            inv.validate(),
            Err(CohortError::TooManyInValues(_))
        ));

        let mut runs = base(CohortEntity::Run);
        runs.filters = vec![CohortFilter::RunIds {
            ids: (0..MAX_RUN_IDS + 1).map(|i| i.to_string()).collect(),
        }];
        assert!(matches!(
            runs.validate(),
            Err(CohortError::TooManyRunIds(_))
        ));

        let mut refs = base(CohortEntity::Step);
        refs.filters = vec![CohortFilter::StepRefs {
            refs: (0..MAX_STEP_REFS + 1)
                .map(|i| StepRef {
                    run_id: i.to_string(),
                    step_ordinal: 0,
                })
                .collect(),
        }];
        assert!(matches!(
            refs.validate(),
            Err(CohortError::TooManyStepRefs(_))
        ));
    }

    #[test]
    fn metric_normalization_matrix() {
        // CacheHitRate only with Absolute.
        let mut chr = base(CohortEntity::Run);
        chr.metric = CohortMetric::CacheHitRate;
        chr.normalization = Normalization::Absolute;
        assert!(chr.validate().is_ok());
        chr.normalization = Normalization::ShareOfSelection;
        assert!(matches!(
            chr.validate(),
            Err(CohortError::InvalidNormalizationForMetric(
                "cache_hit_rate",
                _
            ))
        ));

        // PerOutcome requires a denominator.
        let mut po = base(CohortEntity::Run);
        po.normalization = Normalization::PerOutcome;
        assert_eq!(
            po.validate(),
            Err(CohortError::PerOutcomeMissingDenominator)
        );
        po.outcome_denominator = Some(OutcomeDenominator::Metered {
            kind: MeteredOutcome::PullRequests,
        });
        assert!(po.validate().is_ok());

        let mut unused = base(CohortEntity::Run);
        unused.outcome_denominator = Some(OutcomeDenominator::Metered {
            kind: MeteredOutcome::Commits,
        });
        assert_eq!(
            unused.validate(),
            Err(CohortError::UnexpectedOutcomeDenominator)
        );

        // Empty work-unit name is rejected.
        let mut wu = base(CohortEntity::Run);
        wu.normalization = Normalization::PerOutcome;
        wu.outcome_denominator = Some(OutcomeDenominator::WorkUnit { name: "   ".into() });
        assert_eq!(wu.validate(), Err(CohortError::EmptyWorkUnitName));

        // ShareOfSelection is fine for additive metrics.
        let mut sos = base(CohortEntity::Run);
        sos.metric = CohortMetric::Tokens;
        sos.normalization = Normalization::ShareOfSelection;
        assert!(sos.validate().is_ok());
    }

    #[test]
    fn round_trips_through_serde() {
        let mut spec = base(CohortEntity::Step);
        spec.filters = vec![
            CohortFilter::Eq {
                dimension: CohortDimension::Session,
                value: "s1".into(),
            },
            CohortFilter::QualityRange {
                min: Some(50),
                max: None,
            },
        ];
        spec.pricing = PricingMode::AsOf {
            date: "2026-06-01".into(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        let back: CohortSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    /// The stable canonical JSON + hash for a representative spec. The TypeScript parity test
    /// (`web/test/analysisSerialize.test.ts`) asserts the SAME two strings, proving byte-identical
    /// cross-language canonicalization and hashing.
    #[test]
    fn golden_canonical_and_hash_for_parity() {
        let spec = CohortSpec {
            from: Some("2026-05-01".into()),
            to: None,
            timezone: "America/Los_Angeles".into(),
            entity: CohortEntity::Step,
            filters: vec![
                CohortFilter::In {
                    dimension: CohortDimension::Model,
                    values: vec!["claude-sonnet-4-6".into(), "claude-opus-4-8".into()],
                },
                CohortFilter::Eq {
                    dimension: CohortDimension::Provider,
                    value: "anthropic".into(),
                },
                CohortFilter::GteMicros { value: 1000 },
            ],
            pricing: PricingMode::EffectiveDated,
            metric: CohortMetric::SpendMicros,
            normalization: Normalization::PerRun,
            outcome_denominator: None,
        };
        let canonical = spec.canonical_json();
        let expected = r#"{"entity":"step","filters":[{"dimension":"provider","op":"eq","value":"anthropic"},{"op":"gte_micros","value":1000},{"dimension":"model","op":"in","values":["claude-opus-4-8","claude-sonnet-4-6"]}],"from":"2026-05-01","metric":"spend_micros","normalization":"per_run","outcome_denominator":null,"pricing":{"mode":"effective_dated"},"timezone":"America/Los_Angeles","to":null}"#;
        assert_eq!(canonical, expected);
        // Anchor the exact hash for the TypeScript parity test.
        assert_eq!(spec.cohort_hash(), "f07f3446ba18d3c7");
    }
}
