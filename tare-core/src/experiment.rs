//! CostExperiment: the forward-looking apex — hold the captured task constant, vary a
//! declared axis, and read the cost delta on a shared axis. Every "trial" is an OFFLINE REPRICE of
//! an already-captured run set (no re-execution, no API spend, no payload), using the same token
//! vectors the what-if engine reprices. Because estimates are integer arithmetic over stored
//! counts, the whole grid is enumerated EXHAUSTIVELY (microseconds) rather than sampled — a
//! capability the re-run-to-measure cloud tools structurally lack.
//!
//! Objective: minimize estimated cost. **Quality is a CONSTRAINT the user supplies**, never
//! predicted for a counterfactual cell — so `quality` is `None` on generated cells and the
//! non-dominated (Pareto) set degrades cleanly to the cheapest cell.
//!
//! Pure, clock-free, counts-only. Model swaps carry the what-if tokenizer caveat (`approximate`):
//! a different model tokenizes differently, so a repriced cell holds the captured counts.

use crate::attribute::build_report;
use crate::model::{Provider, RunRecord};
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};

/// A declared dimension of variation. The experiment grid is the Cartesian product of every axis.
/// Model, pricing-snapshot, and cache-strategy axes are all repriced from captured token vectors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "values")]
pub enum Axis {
    /// Reprice every step onto each candidate model id. The reserved value [`Axis::AS_CAPTURED`]
    /// keeps the run's original models for that cell — the baseline point.
    Model(Vec<String>),
    /// Reprice every step under each candidate DATED pricing edition: each value is a
    /// `YYYY-MM-DD` snapshot fed through `PricingTable::as_of`, so a run's captured counts are costed
    /// at that date's rates. [`Axis::AS_CAPTURED`] keeps the given table (the baseline). This is EXACT
    /// (real dated rates × captured counts — no tokenizer swap, no fabrication). Data source: dated
    /// editions imported via `tare pricing refresh`.
    PricingSnapshot(Vec<String>),
    /// Reprice under each cache strategy. [`Axis::AS_CAPTURED`] keeps the captured cache
    /// split (baseline); [`Axis::DECACHE`] folds every cached token (cache_read + both cache-write
    /// windows) back into `fresh_input` and reprices — i.e. "what would this have cost with NO
    /// prompt caching?". EXACT (real rates × recomposed counts, no tokenizer swap), so it directly
    /// answers "did my caching actually save money?" — a decache cell cheaper than baseline means the
    /// cache writes never paid back their premium.
    CacheStrategy(Vec<String>),
}

impl Axis {
    /// Sentinel value meaning "keep the captured models / given pricing for this cell" (the baseline).
    pub const AS_CAPTURED: &'static str = "*as-captured*";
    /// [`Axis::CacheStrategy`] value: reprice with all caching folded away (no prompt caching).
    pub const DECACHE: &'static str = "decache";
}

/// One resolved coordinate value within a cell (parallel to an [`Axis`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "axis", content = "value")]
pub enum AxisValue {
    /// `None` = keep captured models (baseline); `Some(id)` = reprice all steps onto `id`.
    Model(Option<String>),
    /// `None` = keep the given pricing (baseline); `Some(date)` = reprice at that dated edition.
    PricingSnapshot(Option<String>),
    /// `false` = keep the captured cache split (baseline); `true` = decache (fold cached tokens into
    /// fresh_input and reprice).
    CacheStrategy(bool),
}

/// The experiment specification: a set of axes and the objective (minimize cost — the only
/// objective, since quality is a user-supplied constraint, not a computed score).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostExperiment {
    pub axes: Vec<Axis>,
}

/// Optional quality gate for an experiment: "cheapest config among runs that
/// met my quality bar". Applied to the cohort's captured runs by their INGESTED quality scalar —
/// NEVER to counterfactual cells (we can't predict the quality of a model we didn't run). A run with
/// no quality score is excluded when a bound is set (we can't assert it met the bar). Inclusive
/// bounds; either side omitted = unbounded on that side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityConstraint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
}

impl QualityConstraint {
    /// Whether a run's quality `score` satisfies this constraint (inclusive).
    pub fn admits(&self, score: i64) -> bool {
        self.min.is_none_or(|lo| score >= lo) && self.max.is_none_or(|hi| score <= hi)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.min.zip(self.max).is_some_and(|(min, max)| min > max) {
            return Err("quality constraint min must not exceed max".into());
        }
        Ok(())
    }
}

/// A `POST /__tare/experiment` request: run the offline counterfactual grid over the runs a
/// [`CohortSpec`] resolves to, optionally gated by an ingested-quality constraint. `experiment` is
/// the axis grid. Mirrored byte-for-byte by `web/src/analysis/types.ts`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentRequest {
    pub cohort: crate::cohort::CohortSpec,
    pub experiment: CostExperiment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_constraint: Option<QualityConstraint>,
}

/// One evaluated grid cell: a full coordinate + its repriced cost (+ optional user quality).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentCell {
    /// One value per axis, in axis order.
    pub coords: Vec<AxisValue>,
    /// Human-readable coordinate labels, in axis order (e.g. `["model=claude-haiku-4-5"]`).
    pub label: Vec<String>,
    /// Total estimated cost of the whole run set under this cell, in micro-USD.
    pub cost_micros: i64,
    /// User-supplied quality scalar for this cell; `None` in this layer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<i64>,
    /// True if any model was swapped in this cell (the tokenizer caveat applies).
    pub approximate: bool,
}

/// The result of enumerating + repricing the whole grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentResult {
    /// Every grid cell, cheapest first (ties broken by label for determinism).
    pub cells: Vec<ExperimentCell>,
    /// Indices into `cells` of the non-dominated (Pareto) set: minimize cost, maximize quality.
    /// With no quality signal every cell ties on quality, so this is the cheapest cell(s).
    pub pareto: Vec<usize>,
    /// Cost of the as-captured baseline under the given pricing (the point of comparison).
    pub baseline_micros: i64,
    /// Cheapest cell's cost (== `baseline_micros` when nothing beats the baseline).
    pub best_micros: i64,
    /// Savings of the cheapest cell vs the baseline (>= 0; 0 when the baseline is already best).
    pub best_saving_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
    /// True if any cell involved a model swap (its cost carries the tokenizer caveat).
    pub approximate: bool,
}

pub const MAX_EXPERIMENT_AXES: usize = 3;
pub const MAX_AXIS_VALUES: usize = 64;
pub const MAX_EXPERIMENT_CELLS: usize = 4_096;

/// Validate the externally supplied grid before constructing its Cartesian product.
pub fn validate_experiment(exp: &CostExperiment) -> Result<(), String> {
    if exp.axes.len() > MAX_EXPERIMENT_AXES {
        return Err(format!(
            "too many experiment axes: {} (max {MAX_EXPERIMENT_AXES})",
            exp.axes.len()
        ));
    }
    let mut kinds = std::collections::BTreeSet::new();
    let mut cells = 1usize;
    for axis in &exp.axes {
        let (kind, values) = match axis {
            Axis::Model(values) => ("model", values),
            Axis::PricingSnapshot(values) => ("pricing_snapshot", values),
            Axis::CacheStrategy(values) => ("cache_strategy", values),
        };
        if !kinds.insert(kind) {
            return Err(format!("duplicate experiment axis {kind:?}"));
        }
        if values.is_empty() {
            return Err(format!("experiment axis {kind:?} has no values"));
        }
        if values.len() > MAX_AXIS_VALUES {
            return Err(format!(
                "too many values for experiment axis {kind:?}: {} (max {MAX_AXIS_VALUES})",
                values.len()
            ));
        }
        let mut unique = std::collections::BTreeSet::new();
        for value in values {
            if !unique.insert(value) {
                return Err(format!(
                    "duplicate value {value:?} in experiment axis {kind:?}"
                ));
            }
            match axis {
                Axis::Model(_) => {
                    let valid = value == Axis::AS_CAPTURED
                        || (!value.trim().is_empty()
                            && value.chars().count() <= crate::wire::MAX_MODEL_ID_CHARS);
                    if !valid {
                        return Err(format!("invalid model experiment value {value:?}"));
                    }
                }
                Axis::PricingSnapshot(_) => {
                    if value != Axis::AS_CAPTURED {
                        let parsed = crate::calendar::parse_date(value)
                            .filter(|days| crate::calendar::format_date(*days) == *value);
                        if parsed.is_none() {
                            return Err(format!(
                                "invalid pricing snapshot {value:?}; expected YYYY-MM-DD"
                            ));
                        }
                    }
                }
                Axis::CacheStrategy(_) => {
                    if value != Axis::AS_CAPTURED && value != Axis::DECACHE {
                        return Err(format!(
                            "invalid cache strategy {value:?}; expected {:?} or {:?}",
                            Axis::AS_CAPTURED,
                            Axis::DECACHE
                        ));
                    }
                }
            }
        }
        cells = cells
            .checked_mul(values.len())
            .ok_or_else(|| "experiment grid size overflow".to_string())?;
        if cells > MAX_EXPERIMENT_CELLS {
            return Err(format!(
                "experiment grid has {cells} cells (max {MAX_EXPERIMENT_CELLS})"
            ));
        }
    }
    Ok(())
}

/// Enumerate the experiment grid and reprice each cell offline. Unpriced model targets are skipped
/// (a gap, never a fabricated $0), mirroring the estimate-only honesty rule.
pub fn run_experiment(
    runs: &[RunRecord],
    pricing: &PricingTable,
    exp: &CostExperiment,
) -> Result<ExperimentResult, String> {
    validate_experiment(exp)?;
    let baseline_micros = build_report(runs, pricing).total_micros;

    // Expand each axis to its list of concrete values (with the baseline sentinel resolved).
    let per_axis: Vec<Vec<AxisValue>> = exp
        .axes
        .iter()
        .map(|axis| match axis {
            Axis::Model(models) => models
                .iter()
                .map(|m| {
                    if m == Axis::AS_CAPTURED {
                        AxisValue::Model(None)
                    } else {
                        AxisValue::Model(Some(m.clone()))
                    }
                })
                .collect(),
            Axis::PricingSnapshot(dates) => dates
                .iter()
                .map(|d| {
                    if d == Axis::AS_CAPTURED {
                        AxisValue::PricingSnapshot(None)
                    } else {
                        AxisValue::PricingSnapshot(Some(d.clone()))
                    }
                })
                .collect(),
            Axis::CacheStrategy(strategies) => strategies
                .iter()
                .map(|s| AxisValue::CacheStrategy(s == Axis::DECACHE))
                .collect(),
        })
        .collect();

    // Cartesian product of the axis value-lists → one coordinate vector per cell.
    let mut grid: Vec<Vec<AxisValue>> = vec![vec![]];
    for values in &per_axis {
        let capacity = grid
            .len()
            .checked_mul(values.len())
            .ok_or_else(|| "experiment grid size overflow".to_string())?;
        let mut next = Vec::with_capacity(capacity);
        for prefix in &grid {
            for v in values {
                let mut c = prefix.clone();
                c.push(v.clone());
                next.push(c);
            }
        }
        grid = next;
    }

    let mut cells: Vec<ExperimentCell> = Vec::new();
    for coords in grid {
        // Fold the coordinate into a model rewrite + a pricing snapshot + a cache strategy (last of
        // each axis wins).
        let mut target: Option<String> = None;
        let mut snapshot: Option<String> = None;
        let mut decache = false;
        for v in &coords {
            match v {
                AxisValue::Model(m) => target = m.clone(),
                AxisValue::PricingSnapshot(d) => snapshot = d.clone(),
                AxisValue::CacheStrategy(d) => decache = *d,
            }
        }
        // A pricing-snapshot cell reprices at that dated edition (exact — real rates × captured counts);
        // otherwise the given table. `snap` outlives the borrow so we can pass either by reference.
        let snap;
        let cell_pricing: &PricingTable = match &snapshot {
            Some(date) => {
                snap = pricing.as_of(date);
                &snap
            }
            None => pricing,
        };
        // A decache cell folds every cached token back into fresh_input and reprices — exact, no
        // tokenizer swap. `decached` outlives the borrow so we can pass either run set by reference.
        let decached;
        let cell_runs: &[RunRecord] = if decache {
            decached = decache_runs(runs);
            &decached
        } else {
            runs
        };
        let Some((cost_micros, approximate)) = reprice(cell_runs, cell_pricing, target.as_deref())
        else {
            continue; // unpriced target: skip rather than fabricate a $0 cell
        };
        cells.push(ExperimentCell {
            label: label_for(&coords),
            coords,
            cost_micros,
            quality: None,
            approximate,
        });
    }

    cells.sort_by(|a, b| {
        a.cost_micros
            .cmp(&b.cost_micros)
            .then(a.label.cmp(&b.label))
    });

    let pareto = pareto_indices(&cells);
    let best_micros = cells
        .first()
        .map(|c| c.cost_micros)
        .unwrap_or(baseline_micros);
    let approximate = cells.iter().any(|c| c.approximate);

    Ok(ExperimentResult {
        best_saving_micros: baseline_micros.saturating_sub(best_micros).max(0),
        cells,
        pareto,
        baseline_micros,
        best_micros,
        pricing_version: pricing.version.clone(),
        estimated: true,
        approximate,
    })
}

/// Reprice the run set onto an optional target model. `None` = keep captured models. Returns
/// `(cost_micros, approximate)`, or `None` if the target model is unpriced.
fn reprice(
    runs: &[RunRecord],
    pricing: &PricingTable,
    target: Option<&str>,
) -> Option<(i64, bool)> {
    let Some(model) = target else {
        return Some((build_report(runs, pricing).total_micros, false));
    };
    let (to_provider, to_model) = resolve_target(pricing, model)?;
    let approximate = runs.iter().flat_map(|run| &run.steps).any(|step| {
        step.provider != to_provider || step.model != to_model || step.shape.vendor.is_some()
    });
    let hypo: Vec<RunRecord> = runs
        .iter()
        .map(|run| {
            let mut steps = run.steps.clone();
            for s in &mut steps {
                s.provider = to_provider;
                s.model = to_model.clone();
                s.shape.provider = to_provider;
                s.shape.model = to_model.clone();
                s.shape.vendor = None;
            }
            RunRecord {
                run_id: run.run_id.clone(),
                steps,
            }
        })
        .collect();
    Some((build_report(&hypo, pricing).total_micros, approximate))
}

/// Resolve either `model-id` (only when unique across concrete providers) or an explicit
/// `provider/model-id`. Vendor-only OpenAI-compatible rows remain unsupported because the axis has
/// no base-URL/vendor routing contract; skipping them is safer than producing a phantom $0 cell.
fn resolve_target(pricing: &PricingTable, target: &str) -> Option<(Provider, String)> {
    if let Some((provider, model)) = target.split_once('/') {
        if let Some(provider) = Provider::parse(provider) {
            pricing.lookup(provider, None, model)?;
            return Some((provider, model.to_string()));
        }
    }

    let candidates: std::collections::BTreeSet<(Provider, String)> = pricing
        .models
        .iter()
        .filter(|rates| rates.model_id == target)
        .filter_map(|rates| {
            Provider::parse(&rates.provider).map(|provider| (provider, rates.model_id.clone()))
        })
        .collect();
    if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }
}

/// Fold every cached token back into `fresh_input` and zero the cache classes — the
/// "no prompt caching" counterfactual. EXACT: the same real token totals repriced as if none had
/// been cached (cache reads re-sent at full fresh rate, no cache-write premium). Deterministic.
fn decache_runs(runs: &[RunRecord]) -> Vec<RunRecord> {
    runs.iter()
        .map(|run| RunRecord {
            run_id: run.run_id.clone(),
            steps: run
                .steps
                .iter()
                .map(|s| {
                    let mut s = s.clone();
                    let u = &mut s.usage;
                    u.fresh_input = u
                        .fresh_input
                        .saturating_add(u.cache_read)
                        .saturating_add(u.cache_write_5m)
                        .saturating_add(u.cache_write_1h);
                    u.cache_read = 0;
                    u.cache_write_5m = 0;
                    u.cache_write_1h = 0;
                    s
                })
                .collect(),
        })
        .collect()
}

/// One captured run plotted on the cost×quality plane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontierPoint {
    pub run_id: String,
    /// The run's total estimated cost (x-axis), micro-USD.
    pub cost_micros: i64,
    /// The user-supplied quality scalar (y-axis) from, or `None` if unscored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<i64>,
    /// Provenance label recorded with the user-supplied score (`cli`, `header`, `ci`, or `ui`).
    /// Absent for unscored and legacy/source-less points; never inferred from the score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_source: Option<String>,
    /// True if this run is non-dominated (on the cost×quality frontier). With no quality signal
    /// anywhere the frontier degrades to the single cheapest run.
    pub on_frontier: bool,
}

/// The cost×quality frontier over a set of captured runs: cost is the native x-axis,
/// quality the ingested y-axis. The re-run-to-measure cloud tools can't produce this offline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontier {
    /// All runs, cheapest first (ties broken by run_id).
    pub points: Vec<FrontierPoint>,
    /// True if any run carries a quality scalar (otherwise this is a pure-cost frontier).
    pub has_quality: bool,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Build the cost×quality frontier from captured runs and a per-run quality map (run_id → score).
/// Cost is each run's total estimated spend; quality is the ingested scalar (absent → `None`).
/// The non-dominated set minimizes cost and maximizes quality. When scores exist, unscored runs are
/// not quality-ranked and cannot dominate or be dominated by scored points. With no quality signal
/// anywhere the view deliberately degrades to a pure-cost frontier containing the cheapest run(s).
pub fn cost_quality_frontier(
    runs: &[RunRecord],
    pricing: &PricingTable,
    quality: &std::collections::BTreeMap<String, i64>,
) -> Frontier {
    cost_quality_frontier_with_provenance(
        runs,
        pricing,
        quality,
        &std::collections::BTreeMap::new(),
    )
}

/// Build the captured frontier while retaining the recorded provenance label for every
/// user-supplied quality score. `quality_sources` affects display evidence only; dominance uses
/// the score exactly as supplied and never infers or grades a missing value.
pub fn cost_quality_frontier_with_provenance(
    runs: &[RunRecord],
    pricing: &PricingTable,
    quality: &std::collections::BTreeMap<String, i64>,
    quality_sources: &std::collections::BTreeMap<String, String>,
) -> Frontier {
    let mut points: Vec<FrontierPoint> = runs
        .iter()
        .map(|run| {
            let cost_micros = build_report(std::slice::from_ref(run), pricing).total_micros;
            FrontierPoint {
                quality: quality.get(&run.run_id).copied(),
                quality_source: quality
                    .get(&run.run_id)
                    .and_then(|_| quality_sources.get(&run.run_id))
                    .cloned(),
                run_id: run.run_id.clone(),
                cost_micros,
                on_frontier: false,
            }
        })
        .collect();
    points.sort_by(|a, b| {
        a.cost_micros
            .cmp(&b.cost_micros)
            .then(a.run_id.cmp(&b.run_id))
    });

    let has_quality = points.iter().any(|p| p.quality.is_some());
    // Dominance: cheaper-or-equal AND higher-or-equal quality, strictly better on one axis. Missing
    // quality is incomparable when any real score exists; it is never invented as zero.
    for i in 0..points.len() {
        let pi_cost = points[i].cost_micros;
        let Some(pi_q) = points[i].quality else {
            if !has_quality {
                points[i].on_frontier = points
                    .first()
                    .is_some_and(|point| point.cost_micros == pi_cost);
            }
            continue;
        };
        let dominated = points.iter().enumerate().any(|(j, pj)| {
            if i == j {
                return false;
            }
            let Some(pj_q) = pj.quality else {
                return false;
            };
            let no_worse = pj.cost_micros <= pi_cost && pj_q >= pi_q;
            let strictly_better = pj.cost_micros < pi_cost || pj_q > pi_q;
            no_worse && strictly_better
        });
        points[i].on_frontier = !dominated;
    }

    Frontier {
        points,
        has_quality,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

/// Non-dominated set: a cell dominates another when it is no worse on both objectives (lower cost,
/// higher-or-equal quality) and strictly better on at least one. `None` quality is treated as a
/// single shared level, so with no quality signal the frontier collapses to the min-cost cell(s).
fn pareto_indices(cells: &[ExperimentCell]) -> Vec<usize> {
    let q = |c: &ExperimentCell| c.quality.unwrap_or(0);
    (0..cells.len())
        .filter(|&i| {
            let ci = &cells[i];
            !cells.iter().enumerate().any(|(j, cj)| {
                if i == j {
                    return false;
                }
                let no_worse = cj.cost_micros <= ci.cost_micros && q(cj) >= q(ci);
                let strictly_better = cj.cost_micros < ci.cost_micros || q(cj) > q(ci);
                no_worse && strictly_better
            })
        })
        .collect()
}

fn label_for(coords: &[AxisValue]) -> Vec<String> {
    coords
        .iter()
        .map(|v| match v {
            AxisValue::Model(None) => "model=as-captured".to_string(),
            AxisValue::Model(Some(m)) => format!("model={m}"),
            AxisValue::PricingSnapshot(None) => "pricing=as-captured".to_string(),
            AxisValue::PricingSnapshot(Some(d)) => format!("pricing@{d}"),
            AxisValue::CacheStrategy(false) => "cache=as-captured".to_string(),
            AxisValue::CacheStrategy(true) => "cache=decache".to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, RequestShape, StepRecord, UsageTokens};

    // input $3/Mtok, output $15/Mtok; a cheaper model at 1/3 the rates.
    fn pricing() -> PricingTable {
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"big","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000},
              {"provider":"anthropic","model_id":"small","input_micro_per_mtok":1000000,
               "output_micro_per_mtok":5000000,"cache_read_micro_per_mtok":100000,
               "cache_write_5m_micro_per_mtok":1250000,"cache_write_1h_micro_per_mtok":2000000},
              {"provider":"groq","model_id":"vendoronly","input_micro_per_mtok":500000,
               "output_micro_per_mtok":800000,"cache_read_micro_per_mtok":50000,
               "cache_write_5m_micro_per_mtok":600000,"cache_write_1h_micro_per_mtok":900000}]}"#,
        )
        .unwrap()
    }

    fn run() -> Vec<RunRecord> {
        let step = StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "big".into(),
            usage: UsageTokens {
                fresh_input: 1_000_000,
                output: 1_000_000,
                ..Default::default()
            },
            shape: RequestShape {
                model: "big".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: false,
                cached_component: None,
                system_hash: None,
                weights: vec![],
                request_hash: Some(0),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: None,
                workload_key: None,
                effort: None,
                mcp_server: None,
                vendor: None,
                commit: None,
                author: None,
            },
            stop_reason: None,
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        };
        vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step],
        }]
    }

    #[test]
    fn enumerates_model_grid_and_finds_cheapest() {
        let exp = CostExperiment {
            axes: vec![Axis::Model(vec![
                Axis::AS_CAPTURED.to_string(),
                "big".to_string(),
                "small".to_string(),
            ])],
        };
        let res = run_experiment(&run(), &pricing(), &exp).unwrap();
        // baseline "big": 1M input × $3 + 1M output × $15 = $18.
        assert_eq!(res.baseline_micros, 18_000_000);
        assert_eq!(res.cells.len(), 3);
        // "small" is 1/3: $6. Cheapest cell first.
        assert_eq!(res.best_micros, 6_000_000);
        assert_eq!(res.best_saving_micros, 12_000_000);
        assert_eq!(res.cells[0].label, vec!["model=small"]);
        assert!(
            res.cells[0].approximate,
            "a model swap carries the tokenizer caveat"
        );
        // as-captured cell is not approximate.
        let captured = res
            .cells
            .iter()
            .find(|c| c.label == vec!["model=as-captured"])
            .unwrap();
        assert_eq!(captured.cost_micros, 18_000_000);
        assert!(!captured.approximate);
        let unchanged = res
            .cells
            .iter()
            .find(|c| c.label == vec!["model=big"])
            .unwrap();
        assert!(
            !unchanged.approximate,
            "explicitly selecting the already-captured provider/model is not a tokenizer swap"
        );
    }

    #[test]
    fn cache_strategy_axis_decaches_exactly() {
        // a cache-heavy run — 200k fresh, 800k cache-read, 100k cache-write-5m — on "big".
        let mut runs = run();
        runs[0].steps[0].usage = UsageTokens {
            fresh_input: 200_000,
            cache_read: 800_000,
            cache_write_5m: 100_000,
            output: 0,
            ..Default::default()
        };
        let exp = CostExperiment {
            axes: vec![Axis::CacheStrategy(vec![
                Axis::AS_CAPTURED.to_string(),
                Axis::DECACHE.to_string(),
            ])],
        };
        let res = run_experiment(&runs, &pricing(), &exp).unwrap();
        assert_eq!(res.cells.len(), 2);
        // As-captured: 0.2M×$3 + 0.8M×$0.30 + 0.1M×$3.75 = 600k + 240k + 375k = 1.215M.
        assert_eq!(res.baseline_micros, 1_215_000);
        let captured = res
            .cells
            .iter()
            .find(|c| c.label == vec!["cache=as-captured"])
            .unwrap();
        assert_eq!(captured.cost_micros, 1_215_000);
        assert!(
            !captured.approximate,
            "no caching = exact recompute, not a tokenizer estimate"
        );
        // Decache: all 1.1M input tokens at fresh $3 = 3.3M — caching WAS saving money here.
        let decached = res
            .cells
            .iter()
            .find(|c| c.label == vec!["cache=decache"])
            .unwrap();
        assert_eq!(decached.cost_micros, 3_300_000);
        assert!(
            !decached.approximate,
            "decache is an exact recompute of real counts"
        );
        // The baseline is cheaper, so best_saving is 0 (nothing beats keeping the cache).
        assert_eq!(res.best_micros, 1_215_000);
    }

    #[test]
    fn pricing_snapshot_axis_reprices_at_each_dated_edition() {
        // "big" takes a price hike (input $3→$6, output $15→$30) on 2026-07-01.
        let dated = PricingTable::from_json_str(
            r#"{"version":"d","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"big","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":0,
               "cache_write_5m_micro_per_mtok":0,"cache_write_1h_micro_per_mtok":0,
               "effective_date":"2026-06-01"},
              {"provider":"anthropic","model_id":"big","input_micro_per_mtok":6000000,
               "output_micro_per_mtok":30000000,"cache_read_micro_per_mtok":0,
               "cache_write_5m_micro_per_mtok":0,"cache_write_1h_micro_per_mtok":0,
               "effective_date":"2026-07-01"}]}"#,
        )
        .unwrap();
        let exp = CostExperiment {
            axes: vec![Axis::PricingSnapshot(vec![
                "2026-06-15".to_string(),
                "2026-07-15".to_string(),
            ])],
        };
        let res = run_experiment(&run(), &dated, &exp).unwrap();
        // Exact repricing (real dated rates × captured 1M in + 1M out) — no model swap, so NOT approximate.
        let june = res
            .cells
            .iter()
            .find(|c| c.label == vec!["pricing@2026-06-15"])
            .unwrap();
        assert_eq!(june.cost_micros, 18_000_000); // 1M×$3 + 1M×$15
        assert!(
            !june.approximate,
            "a pricing snapshot is exact, not a tokenizer estimate"
        );
        let july = res
            .cells
            .iter()
            .find(|c| c.label == vec!["pricing@2026-07-15"])
            .unwrap();
        assert_eq!(july.cost_micros, 36_000_000); // 1M×$6 + 1M×$30
    }

    #[test]
    fn pareto_is_the_cheapest_cell_without_quality() {
        let exp = CostExperiment {
            axes: vec![Axis::Model(vec!["big".to_string(), "small".to_string()])],
        };
        let res = run_experiment(&run(), &pricing(), &exp).unwrap();
        assert_eq!(
            res.pareto.len(),
            1,
            "no quality → frontier is the single cheapest"
        );
        assert_eq!(res.cells[res.pareto[0]].label, vec!["model=small"]);
    }

    #[test]
    fn quality_constraint_yields_a_two_point_frontier() {
        // Manually attach a caller-supplied quality signal and re-run the Pareto routine:
        // the pricier cell survives only because it scores higher on quality.
        let mut cells = vec![
            ExperimentCell {
                coords: vec![AxisValue::Model(Some("small".into()))],
                label: vec!["model=small".into()],
                cost_micros: 6_000_000,
                quality: Some(70),
                approximate: true,
            },
            ExperimentCell {
                coords: vec![AxisValue::Model(Some("big".into()))],
                label: vec!["model=big".into()],
                cost_micros: 18_000_000,
                quality: Some(95),
                approximate: true,
            },
        ];
        cells.sort_by_key(|a| a.cost_micros);
        let pareto = pareto_indices(&cells);
        assert_eq!(
            pareto.len(),
            2,
            "cheap-but-worse and dear-but-better both non-dominated"
        );
    }

    #[test]
    fn unpriced_target_is_skipped_not_zeroed() {
        let exp = CostExperiment {
            axes: vec![Axis::Model(vec!["small".to_string(), "ghost".to_string()])],
        };
        let res = run_experiment(&run(), &pricing(), &exp).unwrap();
        assert_eq!(
            res.cells.len(),
            1,
            "the unpriced 'ghost' cell is dropped, never rendered $0"
        );
        assert_eq!(res.cells[0].label, vec!["model=small"]);
    }

    #[test]
    fn vendor_only_target_is_skipped_not_zeroed() {
        // `vendoronly` is priced only under an OpenAI-compatible vendor row (provider "groq"), which
        // the reprice path can't target — its cell must be DROPPED, not rendered as a $0 (~100%
        // phantom saving). Mirrors whatif/estimate's guard.
        let exp = CostExperiment {
            axes: vec![Axis::Model(vec![
                "small".to_string(),
                "vendoronly".to_string(),
            ])],
        };
        let res = run_experiment(&run(), &pricing(), &exp).unwrap();
        assert_eq!(
            res.cells.len(),
            1,
            "the vendor-only cell is dropped, never $0"
        );
        assert_eq!(res.cells[0].label, vec!["model=small"]);
    }

    #[test]
    fn experiment_input_validation_bounds_and_disambiguates_the_grid() {
        assert!(run_experiment(
            &run(),
            &pricing(),
            &CostExperiment {
                axes: vec![Axis::CacheStrategy(vec!["mystery".into()])]
            }
        )
        .unwrap_err()
        .contains("invalid cache strategy"));
        assert!(run_experiment(
            &run(),
            &pricing(),
            &CostExperiment {
                axes: vec![Axis::PricingSnapshot(vec!["2026-2-3".into()])]
            }
        )
        .unwrap_err()
        .contains("invalid pricing snapshot"));
        assert!(run_experiment(
            &run(),
            &pricing(),
            &CostExperiment {
                axes: vec![Axis::Model(vec![])]
            }
        )
        .unwrap_err()
        .contains("has no values"));
        assert!(QualityConstraint {
            min: Some(10),
            max: Some(9)
        }
        .validate()
        .is_err());
    }

    #[test]
    fn provider_qualified_model_target_resolves_ambiguity() {
        let ambiguous = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"shared","input_micro_per_mtok":1000000,
               "output_micro_per_mtok":1000000,"cache_read_micro_per_mtok":100000,
               "cache_write_5m_micro_per_mtok":1250000,"cache_write_1h_micro_per_mtok":2000000},
              {"provider":"openai","model_id":"shared","input_micro_per_mtok":2000000,
               "output_micro_per_mtok":2000000,"cache_read_micro_per_mtok":200000,
               "cache_write_5m_micro_per_mtok":2500000,"cache_write_1h_micro_per_mtok":4000000},
              {"provider":"anthropic","model_id":"big","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000}]}"#,
        )
        .unwrap();
        let result = run_experiment(
            &run(),
            &ambiguous,
            &CostExperiment {
                axes: vec![Axis::Model(vec!["shared".into(), "openai/shared".into()])],
            },
        )
        .unwrap();
        assert_eq!(result.cells.len(), 1, "ambiguous bare target is skipped");
        assert_eq!(result.cells[0].label, vec!["model=openai/shared"]);
        assert_eq!(result.cells[0].cost_micros, 4_000_000);
        assert!(result.cells[0].approximate);
    }

    fn run_costing(run_id: &str, model: &str) -> RunRecord {
        let step = StepRecord {
            run_id: run_id.into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: model.into(),
            usage: UsageTokens {
                fresh_input: 1_000_000,
                output: 1_000_000,
                ..Default::default()
            },
            shape: RequestShape {
                model: model.into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: false,
                cached_component: None,
                system_hash: None,
                weights: vec![],
                request_hash: Some(0),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: None,
                workload_key: None,
                effort: None,
                mcp_server: None,
                vendor: None,
                commit: None,
                author: None,
            },
            stop_reason: None,
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        };
        RunRecord {
            run_id: run_id.into(),
            steps: vec![step],
        }
    }

    #[test]
    fn frontier_without_quality_is_the_cheapest_run() {
        let runs = vec![run_costing("cheap", "small"), run_costing("dear", "big")];
        let f = cost_quality_frontier(&runs, &pricing(), &std::collections::BTreeMap::new());
        assert!(!f.has_quality);
        assert_eq!(f.points[0].run_id, "cheap", "cheapest first");
        assert!(
            f.points[0].on_frontier,
            "cheapest is the sole frontier point"
        );
        assert!(
            !f.points[1].on_frontier,
            "dearer + no better quality is dominated"
        );
    }

    #[test]
    fn frontier_with_quality_keeps_the_dear_but_better_run() {
        let runs = vec![run_costing("cheap", "small"), run_costing("dear", "big")];
        let mut q = std::collections::BTreeMap::new();
        q.insert("cheap".to_string(), 60); // cheap but lower quality
        q.insert("dear".to_string(), 95); // dear but higher quality
        let f = cost_quality_frontier(&runs, &pricing(), &q);
        assert!(f.has_quality);
        // Both are non-dominated: neither beats the other on both axes.
        assert!(f.points.iter().all(|p| p.on_frontier));
    }

    #[test]
    fn frontier_preserves_user_quality_source_without_inference() {
        let runs = vec![
            run_costing("scored", "small"),
            run_costing("unscored", "big"),
        ];
        let quality = std::collections::BTreeMap::from([("scored".to_string(), 91)]);
        let sources = std::collections::BTreeMap::from([
            ("scored".to_string(), "ci".to_string()),
            // A source without a score is not evidence and must not be attached.
            ("unscored".to_string(), "header".to_string()),
        ]);
        let frontier = cost_quality_frontier_with_provenance(&runs, &pricing(), &quality, &sources);
        let scored = frontier
            .points
            .iter()
            .find(|point| point.run_id == "scored")
            .unwrap();
        let unscored = frontier
            .points
            .iter()
            .find(|point| point.run_id == "unscored")
            .unwrap();
        assert_eq!(scored.quality, Some(91));
        assert_eq!(scored.quality_source.as_deref(), Some("ci"));
        assert_eq!(unscored.quality, None);
        assert_eq!(unscored.quality_source, None);
        let json = serde_json::to_value(&frontier).unwrap();
        let wire_points = json["points"].as_array().unwrap();
        let scored_wire = wire_points
            .iter()
            .find(|point| point["run_id"] == "scored")
            .unwrap();
        let unscored_wire = wire_points
            .iter()
            .find(|point| point["run_id"] == "unscored")
            .unwrap();
        assert_eq!(scored_wire["quality_source"], "ci");
        assert!(unscored_wire.get("quality_source").is_none());
    }

    #[test]
    fn unscored_run_is_not_invented_as_zero_on_a_scored_frontier() {
        let runs = vec![
            run_costing("cheap-unscored", "small"),
            run_costing("scored", "big"),
        ];
        // Negative is a valid value on the user's arbitrary scale. Treating missing as numeric zero
        // would incorrectly let the cheap unscored run dominate this real observation.
        let quality = std::collections::BTreeMap::from([("scored".to_string(), -1)]);
        let frontier = cost_quality_frontier(&runs, &pricing(), &quality);
        let unscored = frontier
            .points
            .iter()
            .find(|point| point.run_id == "cheap-unscored")
            .unwrap();
        let scored = frontier
            .points
            .iter()
            .find(|point| point.run_id == "scored")
            .unwrap();
        assert!(
            !unscored.on_frontier,
            "unscored is explicitly not quality-ranked"
        );
        assert!(
            scored.on_frontier,
            "a missing value cannot dominate a real score"
        );
    }

    #[test]
    fn frontier_drops_a_dear_and_worse_run() {
        let runs = vec![
            run_costing("cheap", "small"),
            run_costing("dear", "big"),
            run_costing("mid", "big"),
        ];
        let mut q = std::collections::BTreeMap::new();
        q.insert("cheap".to_string(), 90);
        q.insert("dear".to_string(), 50); // dearer AND worse than cheap → dominated
        q.insert("mid".to_string(), 95); // dear but best quality → survives
        let f = cost_quality_frontier(&runs, &pricing(), &q);
        let dear = f.points.iter().find(|p| p.run_id == "dear").unwrap();
        assert!(!dear.on_frontier, "dominated by cheap (cheaper AND better)");
        assert!(
            f.points
                .iter()
                .find(|p| p.run_id == "cheap")
                .unwrap()
                .on_frontier
        );
        assert!(
            f.points
                .iter()
                .find(|p| p.run_id == "mid")
                .unwrap()
                .on_frontier
        );
    }

    #[test]
    fn empty_axes_yields_a_single_baseline_cell() {
        let res = run_experiment(&run(), &pricing(), &CostExperiment { axes: vec![] }).unwrap();
        assert_eq!(res.cells.len(), 1);
        assert_eq!(res.cells[0].cost_micros, res.baseline_micros);
        assert!(!res.approximate);
    }
}
