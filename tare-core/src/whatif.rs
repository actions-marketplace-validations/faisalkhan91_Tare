//! "What-if" model/tier swap: reprice the CAPTURED token vectors against a different model's
//! rates, with no re-tokenization or replay. Output is a `ReportDiff` (baseline vs
//! hypothetical) — pure, integer, deterministic.
//!
//! A swap to a different model is stamped `approximate_tokenizer` because the real token
//! counts WOULD differ on another tokenizer — we hold the captured counts), and a swap across
//! providers additionally `approximate_cross_provider` (cache-axis semantics differ, e.g.
//! Anthropic's 5m/1h split vs OpenAI's zeros — repricing reshapes cause structure, not just
//! totals). An unpriced target HARD-ERRORS rather than silently rendering a hypothetical as $0.

use crate::account::cost_usage;
use crate::attribute::{build_report, Report};
use crate::diff::{diff_reports, ReportDiff};
use crate::model::{Provider, RunRecord, UsageTokens};
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A requested swap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Swap {
    /// Swap steps whose model is `from` to model `to`.
    Model { from: String, to: String },
    /// Swap every step to model `to`.
    AllTo { to: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSwap {
    pub from: String,
    pub to_provider: String,
    pub to_model: String,
    /// The captured token counts are kept; on a different model the real counts would differ.
    pub approximate_tokenizer: bool,
    /// Target provider differs from the source — cache-axis semantics may differ too.
    pub approximate_cross_provider: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatIfReport {
    pub swaps: Vec<ResolvedSwap>,
    pub diff: ReportDiff,
    pub estimated: bool,
    /// True if ANY swap is approximate (so consumers can surface the caveat prominently).
    pub approximate: bool,
}

/// One ranked recommendation: swap everything to this priced model and the resulting delta.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recommendation {
    pub to_provider: String,
    pub to_model: String,
    pub total_after_micros: i64,
    pub delta_micros: i64,
    /// Always true: captured counts are kept, while a different model tokenizes differently.
    pub approximate_tokenizer: bool,
    /// True when the target provider differs from a source provider in the runs.
    pub approximate_cross_provider: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatIfRecommendations {
    pub baseline_micros: i64,
    /// Cheaper-first (delta asc), ties by model id. Only PRICED targets — never a $0 phantom.
    pub recommendations: Vec<Recommendation>,
    pub estimated: bool,
    /// Every recommendation is approximate; no swap is ever labeled "exact".
    pub approximate: bool,
}

/// Rank every priced model by what the captured runs WOULD cost on it (swap-all-to). Cheaper
/// first. With `cross_provider == false`, only targets whose provider already appears in the
/// runs are considered. NOTHING is labeled exact; unpriced models are simply never candidates
/// because they are not in the table, so there is no $0 phantom.
pub fn recommend(
    runs: &[RunRecord],
    pricing: &PricingTable,
    cross_provider: bool,
) -> WhatIfRecommendations {
    use std::collections::BTreeSet;
    let present: BTreeSet<&str> = runs
        .iter()
        .flat_map(|r| r.steps.iter().map(|s| s.provider.as_str()))
        .collect();
    // Build the baseline attribution ONCE and reuse it for every candidate — it's
    // identical across swaps, so the previous per-candidate whatif() rebuild was M-1 wasted passes.
    let baseline = build_report(runs, pricing);

    let mut recs = Vec::new();
    let mut seen = BTreeSet::new();
    for m in &pricing.models {
        if !seen.insert((m.provider.clone(), m.model_id.clone())) {
            continue; // dedup the indexed table
        }
        // OpenAI-compatible vendor rows key on a free-text vendor (e.g. "groq") that isn't a
        // Provider enum tag; the swap path can't reprice to them without vendor plumbing, so they
        // aren't recommend targets yet (vendor-as-swap-target is future work). Skip them.
        if Provider::parse(&m.provider).is_none() {
            continue;
        }
        if !cross_provider && !present.contains(m.provider.as_str()) {
            continue;
        }
        // Reuse the single-swap path (same approximate stamps + repricing), but against the
        // baseline computed once above — not a fresh full pass per candidate.
        if let Ok(r) = whatif_with_baseline(
            runs,
            pricing,
            &[Swap::AllTo {
                to: m.model_id.clone(),
            }],
            &baseline,
        ) {
            let s = &r.swaps[0];
            recs.push(Recommendation {
                to_provider: s.to_provider.clone(),
                to_model: s.to_model.clone(),
                total_after_micros: r.diff.total_after,
                delta_micros: r.diff.delta_micros,
                approximate_tokenizer: s.approximate_tokenizer,
                approximate_cross_provider: s.approximate_cross_provider,
            });
        }
    }
    recs.sort_by(|a, b| {
        a.delta_micros
            .cmp(&b.delta_micros)
            .then(a.to_model.cmp(&b.to_model))
    });
    // Dedup by the RENDERED (to_provider, to_model): the pricing table carries some
    // models under two provider labels (e.g. `gpt-5` as both `openai` and `azure_openai`) whose swap
    // path resolves to the same `to_provider`, so the raw loop emitted visually-identical twin rows.
    // The pricing-table `seen` key (its own provider) can't catch that — dedup the OUTPUT the user
    // reads. Sorted first, so the kept row is the deterministic lowest-delta one.
    {
        let mut out_seen = BTreeSet::new();
        recs.retain(|r| out_seen.insert((r.to_provider.clone(), r.to_model.clone())));
    }
    WhatIfRecommendations {
        baseline_micros: baseline.total_micros,
        recommendations: recs,
        estimated: true,
        approximate: true,
    }
}

/// Pick the target rates row for a swap's `to` model, or hard-error if it isn't priced.
fn resolve_target<'a>(
    pricing: &'a PricingTable,
    to: &str,
) -> Result<&'a crate::pricing::ModelRates, String> {
    let target = pricing.find_by_model(to).ok_or_else(|| {
        format!("what-if target model {to:?} has no bundled price; refusing to render it as $0")
    })?;
    // The swap path rewrites (provider, model) but has NO vendor plumbing, so it can only reprice
    // to targets whose provider tag is a real `Provider` enum. An OpenAI-compatible *vendor* row
    // (provider = "groq"/"together") would leave each rerouted step on its ORIGINAL
    // provider with a model that provider can't price → unpriced → silently dropped from the total,
    // fabricating an approximately 100% "saving". Refuse it rather than render a $0 phantom. Mirrors the
    // `recommend()` skip. Fixes both `whatif` and `route_whatif` at the one choke point.
    if Provider::parse(&target.provider).is_none() {
        return Err(format!(
            "what-if target {to:?} is only priced under OpenAI-compatible vendor {:?}; vendor \
             swap targets aren't supported yet (repricing would silently drop them as $0)",
            target.provider
        ));
    }
    Ok(target)
}

/// Reprice `runs` under `swaps` and return the baseline-vs-hypothetical diff. Errors if any
/// swap target is unpriced.
pub fn whatif(
    runs: &[RunRecord],
    pricing: &PricingTable,
    swaps: &[Swap],
) -> Result<WhatIfReport, String> {
    let baseline = build_report(runs, pricing);
    whatif_with_baseline(runs, pricing, swaps, &baseline)
}

/// As [`whatif`], but reusing an already-computed baseline `Report`. `recommend` prices M candidate
/// models against the SAME baseline — this variant lets it build the baseline ONCE instead of
/// rebuilding the identical full-attribution pass M times.
pub(crate) fn whatif_with_baseline(
    runs: &[RunRecord],
    pricing: &PricingTable,
    swaps: &[Swap],
    baseline: &Report,
) -> Result<WhatIfReport, String> {
    // Resolve every target up front (hard-error on unpriced) and record approximation stamps.
    let mut resolved: Vec<(Option<String>, String, ResolvedSwap)> = Vec::new();
    for sw in swaps {
        let (from_match, to) = match sw {
            Swap::Model { from, to } => (Some(from.clone()), to.clone()),
            Swap::AllTo { to } => (None, to.clone()),
        };
        let target = resolve_target(pricing, &to)?;
        resolved.push((
            from_match,
            target.model_id.clone(),
            ResolvedSwap {
                from: match sw {
                    Swap::Model { from, .. } => from.clone(),
                    Swap::AllTo { .. } => "*".to_string(),
                },
                to_provider: target.provider.clone(),
                to_model: target.model_id.clone(),
                approximate_tokenizer: true,
                approximate_cross_provider: false, // filled per-step below if it ever fires
            },
        ));
    }

    // baseline is provided by the caller (computed once) — no per-call rebuild.

    // Rewrite each step's (provider, model) to its swap target; the captured token vector is
    // unchanged, so build_report reprices it against the target's rates.
    let mut cross_provider_seen = vec![false; resolved.len()];
    let mut hypo_runs: Vec<RunRecord> = Vec::with_capacity(runs.len());
    for run in runs {
        let mut steps = run.steps.clone();
        for step in &mut steps {
            // First matching swap wins (AllTo matches anything).
            for (i, (from_match, target_model, rs)) in resolved.iter().enumerate() {
                let matches = match from_match {
                    Some(m) => &step.model == m,
                    None => true,
                };
                if matches {
                    if let Some(tp) = Provider::parse(&rs.to_provider) {
                        if tp != step.provider {
                            cross_provider_seen[i] = true;
                        }
                        step.provider = tp;
                    }
                    step.model = target_model.clone();
                    break;
                }
            }
        }
        hypo_runs.push(RunRecord {
            run_id: run.run_id.clone(),
            steps,
        });
    }

    let hypothetical = build_report(&hypo_runs, pricing);
    let diff = diff_reports(baseline, &hypothetical);

    let mut swaps_out: Vec<ResolvedSwap> = resolved.into_iter().map(|(_, _, rs)| rs).collect();
    for (i, rs) in swaps_out.iter_mut().enumerate() {
        rs.approximate_cross_provider = cross_provider_seen[i];
    }
    let approximate = swaps_out
        .iter()
        .any(|s| s.approximate_tokenizer || s.approximate_cross_provider);
    Ok(WhatIfReport {
        swaps: swaps_out,
        diff,
        estimated: true,
        approximate,
    })
}

/// A conditional model-routing policy: reroute only the steps that match a predicate
/// to a cheaper model, keeping every other step on its captured model. The canonical rule is
/// "small-output calls don't need a premium model" — `when_output_below` routes any step whose
/// captured output tokens are strictly below the threshold to `to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutingPolicy {
    /// Route matching steps to this model id (must be priced — unpriced hard-errors, like `whatif`).
    pub to: String,
    /// Match steps whose output tokens are strictly below this (e.g. 500 → "<500 out-tok → Haiku").
    pub when_output_below: u64,
}

/// Reprice ALL captured runs under a routing policy: matching steps are re-costed on `to`'s rates
/// (the captured token vector is HELD — no re-tokenization/replay), non-matching steps stay on
/// their own model. Returns the baseline-vs-routed [`WhatIfReport`].
///
/// Honesty (#6, same contract as [`whatif`]): repricing across models is **not** a pure multiply —
/// a different tokenizer yields different counts and per-model tool-overhead differs — so the result
/// is always `approximate_tokenizer`, and `approximate_cross_provider` when the target provider
/// differs from a rerouted step's. An unpriced `to` HARD-ERRORS rather than rendering a $0 phantom.
/// A step already on `to` is left untouched (no self-swap).
pub fn route_whatif(
    runs: &[RunRecord],
    pricing: &PricingTable,
    policy: &RoutingPolicy,
) -> Result<WhatIfReport, String> {
    let target = resolve_target(pricing, &policy.to)?;
    let to_provider = target.provider.clone();
    let to_model = target.model_id.clone();
    let target_provider = Provider::parse(&to_provider);

    let baseline: Report = build_report(runs, pricing);

    let mut cross_provider_seen = false;
    let mut hypo_runs: Vec<RunRecord> = Vec::with_capacity(runs.len());
    for run in runs {
        let mut steps = run.steps.clone();
        for step in &mut steps {
            let matches = step.usage.output < policy.when_output_below && step.model != to_model;
            if !matches {
                continue;
            }
            if let Some(tp) = target_provider {
                if tp != step.provider {
                    cross_provider_seen = true;
                }
                step.provider = tp;
            }
            step.model = to_model.clone();
        }
        hypo_runs.push(RunRecord {
            run_id: run.run_id.clone(),
            steps,
        });
    }

    let hypothetical = build_report(&hypo_runs, pricing);
    let diff = diff_reports(&baseline, &hypothetical);

    let swap = ResolvedSwap {
        from: format!("* (output<{})", policy.when_output_below),
        to_provider,
        to_model,
        approximate_tokenizer: true,
        approximate_cross_provider: cross_provider_seen,
    };
    Ok(WhatIfReport {
        swaps: vec![swap],
        diff,
        estimated: true,
        approximate: true,
    })
}

// ---- per-step rightsizing / downshift waste ----

/// Output-token ceiling below which a call counts as "simple" (a likely single-shot task that
/// probably didn't need a premium model). Deterministic, conservative.
pub const SIMPLE_OUTPUT_MAX: u64 = 500;

/// Fresh-input ceiling for a "simple" step. A large-context single-shot (e.g.
/// a 1M-token prompt with a short answer + no cache) is NOT a downshift candidate — the input, not
/// the model's reasoning, is the cost, and a cheaper model may not handle the context. Without this
/// the rightsizing ledger booked the full premium×large-input delta as "recoverable" — a big
/// over-claim. 50k fresh input comfortably covers genuinely-simple prompts.
pub const SIMPLE_INPUT_MAX: u64 = 50_000;

/// A "simple" step: little output AND little fresh input AND no cache activity (no cache-read means
/// it isn't a deep multi-turn continuation; no cache-write means it isn't seeding a long context).
/// Premium models are wasted on these. Pure threshold check.
fn is_simple_task(u: &UsageTokens) -> bool {
    u.output < SIMPLE_OUTPUT_MAX
        && u.fresh_input < SIMPLE_INPUT_MAX
        && u.cache_read == 0
        && u.cache_write_5m == 0
        && u.cache_write_1h == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownshiftRow {
    pub provider: String,
    pub from_model: String,
    /// The cheapest same-provider model these simple steps would reprice onto.
    pub to_model: String,
    pub steps: u32,
    /// Recoverable micro-USD = current cost − cheapest-same-provider cost, summed over the simple
    /// steps. Approximate (a different model tokenizes differently); keeps the captured counts.
    pub micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownshiftReport {
    pub rows: Vec<DownshiftRow>,
    pub total_micros: i64,
    /// Always true: repricing onto a different model cannot reuse the source tokenizer.
    pub approximate_tokenizer: bool,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Per-step rightsizing: for each "simple" step on a model that has a cheaper same-provider
/// alternative, reprice ONLY that step onto the cheapest same-provider model and recover the delta.
/// Unlike `recommend` (one wholesale fleet swap), this targets the over-powered calls a complexity-
/// aware router would downshift. Pure function of captured token vectors + pricing; no
/// re-tokenization (stamped `approximate_tokenizer`).
///
/// Operational proxies for the spec's "premium model, simple, single-turn": **premium** ≈ "has a
/// strictly cheaper same-provider model" (there is no capability tier in pricing — `ModelRates`
/// tiers encode input size, not capability); **single-turn** ≈ "no cache activity" (a cache-read is
/// a deep continuation; a cache-write is seeding a long context — both conservatively excluded).
/// Both the captured baseline and every alternative are intentionally priced against the current
/// edition. This isolates the model-choice delta instead of mixing it with historical price changes.
pub fn downshift_waste(runs: &[RunRecord], pricing: &PricingTable) -> DownshiftReport {
    // (provider_wire, from_model) -> (recoverable, steps, target_model -> recoverable)
    type Agg = BTreeMap<(String, String), (i64, u32, BTreeMap<String, i64>)>;
    let mut agg: Agg = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            if !is_simple_task(&step.usage) {
                continue;
            }
            let Some(cur) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            let cur_cost = cost_usage(&step.usage, cur, &step.shape).total.micros();
            // Same "provider" in the pricing-table sense: the vendor label for an OpenAI-compatible
            // step (its rows are tagged "groq"/"together", not "openai_compatible").
            let prov = step.provider.pricing_key(step.shape.vendor.as_deref());
            // Cheapest OTHER same-provider model for this exact captured usage.
            let mut best: Option<(String, i64)> = None;
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for m in &pricing.models {
                if m.provider != prov.as_ref()
                    || m.model_id == step.model
                    || !seen.insert(&m.model_id)
                {
                    continue;
                }
                if let Some(r) =
                    pricing.lookup(step.provider, step.shape.vendor.as_deref(), &m.model_id)
                {
                    let c = cost_usage(&step.usage, r, &step.shape).total.micros();
                    if best.as_ref().is_none_or(|(_, bc)| c < *bc) {
                        best = Some((m.model_id.clone(), c));
                    }
                }
            }
            if let Some((to, to_cost)) = best {
                let recov = cur_cost - to_cost;
                if recov > 0 {
                    let e = agg
                        .entry((prov.to_string(), step.model.clone()))
                        .or_insert((0, 0, BTreeMap::new()));
                    e.0 = e.0.saturating_add(recov);
                    e.1 = e.1.saturating_add(1);
                    let target_total = e.2.entry(to).or_insert(0);
                    *target_total = target_total.saturating_add(recov);
                }
            }
        }
    }
    let mut rows: Vec<DownshiftRow> = agg
        .into_iter()
        .map(|((provider, from_model), (micros, steps, targets))| {
            // Representative target = the model capturing the most recoverable (deterministic).
            let to_model = targets
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(t, _)| t)
                .unwrap_or_default();
            DownshiftRow {
                provider,
                from_model,
                to_model,
                steps,
                micros,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.micros
            .cmp(&a.micros)
            .then(a.from_model.cmp(&b.from_model))
    });
    let total_micros = rows
        .iter()
        .map(|r| r.micros)
        .fold(0i64, i64::saturating_add);
    DownshiftReport {
        rows,
        total_micros,
        approximate_tokenizer: true,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_step;

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    fn opus_run() -> Vec<RunRecord> {
        let req = include_bytes!("../../fixtures/anthropic_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_nonstream/response.json");
        let step = ingest_step("r", 1, Provider::Anthropic, req, resp).unwrap();
        vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step],
        }]
    }

    #[test]
    fn swapping_opus_to_haiku_lowers_the_estimate() {
        // claude-opus-4-8 -> claude-haiku-4-5 (same provider): cheaper, same tokenizer family.
        let runs = opus_run();
        let r = whatif(
            &runs,
            &pricing(),
            &[Swap::Model {
                from: "claude-opus-4-8".into(),
                to: "claude-haiku-4-5".into(),
            }],
        )
        .unwrap();
        assert!(r.diff.delta_micros < 0, "haiku is cheaper than opus");
        assert!(r.swaps[0].approximate_tokenizer);
        assert!(!r.swaps[0].approximate_cross_provider); // same provider
        assert_eq!(r.swaps[0].to_model, "claude-haiku-4-5");
    }

    #[test]
    fn routing_policy_reroutes_only_small_output_steps() {
        // Two opus steps: one small-output (routes), one large-output (stays).
        let mut runs = opus_run();
        runs[0].steps[0].usage.output = 100; // < 500 -> routes to haiku
        let mut big = runs[0].steps[0].clone();
        big.step_ordinal = 2;
        big.usage.output = 5_000; // >= 500 -> stays on opus
        runs[0].steps.push(big);

        let policy = RoutingPolicy {
            to: "claude-haiku-4-5".into(),
            when_output_below: 500,
        };
        let r = route_whatif(&runs, &pricing(), &policy).unwrap();
        assert!(
            r.diff.delta_micros < 0,
            "rerouting the small call is cheaper"
        );
        assert!(
            r.approximate,
            "cross-tokenizer counterfactual is approximate"
        );
        assert_eq!(r.swaps[0].to_model, "claude-haiku-4-5");
        assert_eq!(r.swaps[0].from, "* (output<500)");
        assert!(!r.swaps[0].approximate_cross_provider, "same provider");

        // A policy that matches nothing (threshold 0) is a valid zero-delta report.
        let none = RoutingPolicy {
            to: "claude-haiku-4-5".into(),
            when_output_below: 0,
        };
        assert_eq!(
            route_whatif(&runs, &pricing(), &none)
                .unwrap()
                .diff
                .delta_micros,
            0
        );

        // Unpriced target hard-errors (never a $0 phantom).
        let bad = RoutingPolicy {
            to: "totally-made-up-model".into(),
            when_output_below: 500,
        };
        assert!(route_whatif(&runs, &pricing(), &bad).is_err());

        // A model priced ONLY under an OpenAI-compatible vendor row (provider "groq", not a
        // Provider enum) must hard-error, NOT silently drop the rerouted step as unpriced and
        // report a fabricated ~100% saving. The swap path has no vendor
        // plumbing, so it can't reprice onto a vendor target.
        let vendor = RoutingPolicy {
            to: "llama-3.1-70b".into(),
            when_output_below: 500,
        };
        let err = route_whatif(&runs, &pricing(), &vendor);
        assert!(
            err.is_err(),
            "vendor-only target must be refused, not rendered as $0"
        );
        // Same guard covers the plain swap path.
        assert!(whatif(
            &runs,
            &pricing(),
            &[Swap::AllTo {
                to: "llama-3.1-70b".into()
            }]
        )
        .is_err());
    }

    #[test]
    fn downshift_flags_a_simple_premium_call_and_ignores_complex_ones() {
        // A small, cache-less call on premium opus → repriceable onto a cheaper same-provider model.
        let mut runs = opus_run();
        {
            let u = &mut runs[0].steps[0].usage;
            u.output = 100;
            u.fresh_input = 2000;
            u.cache_read = 0;
            u.cache_write_5m = 0;
            u.cache_write_1h = 0;
        }
        let rep = downshift_waste(&runs, &pricing());
        assert_eq!(rep.rows.len(), 1);
        let row = &rep.rows[0];
        assert_eq!(
            (row.provider.as_str(), row.from_model.as_str()),
            ("anthropic", "claude-opus-4-8")
        );
        assert_eq!(row.steps, 1);
        assert!(row.micros > 0, "a cheaper same-provider model exists");
        assert_ne!(
            row.to_model, "claude-opus-4-8",
            "downshifts to a different model"
        );
        assert!(rep.approximate_tokenizer);

        // A large-output call is NOT simple → no rightsizing.
        let mut complex = opus_run();
        complex[0].steps[0].usage.output = 5_000;
        assert!(downshift_waste(&complex, &pricing()).rows.is_empty());

        // A cache-using call (multi-turn continuation) is NOT simple either.
        let mut cached = opus_run();
        {
            let u = &mut cached[0].steps[0].usage;
            u.output = 100;
            u.cache_read = 4_000;
        }
        assert!(downshift_waste(&cached, &pricing()).rows.is_empty());
    }

    #[test]
    fn downshift_ignores_a_model_with_no_cheaper_sibling_and_aggregates_steps() {
        // A simple call already on the cheapest same-provider model has nothing to downshift to.
        let mut cheapest = opus_run();
        cheapest[0].steps[0].model = "claude-haiku-4-5".into();
        {
            let u = &mut cheapest[0].steps[0].usage;
            u.output = 50;
            u.fresh_input = 1000;
            u.cache_read = 0;
            u.cache_write_5m = 0;
            u.cache_write_1h = 0;
        }
        assert!(
            downshift_waste(&cheapest, &pricing()).rows.is_empty(),
            "no strictly-cheaper same-provider model -> no rightsizing"
        );

        // Two simple opus steps aggregate into one row with steps == 2.
        let mut two = opus_run();
        let s0 = two[0].steps[0].clone();
        two[0].steps.push(s0);
        for s in &mut two[0].steps {
            s.usage.output = 100;
            s.usage.fresh_input = 2000;
            s.usage.cache_read = 0;
            s.usage.cache_write_5m = 0;
            s.usage.cache_write_1h = 0;
        }
        let rep = downshift_waste(&two, &pricing());
        assert_eq!(rep.rows.len(), 1);
        assert_eq!(rep.rows[0].steps, 2);
    }

    #[test]
    fn cross_provider_swap_is_flagged_approximate() {
        let runs = opus_run();
        let r = whatif(
            &runs,
            &pricing(),
            &[Swap::AllTo {
                to: "gpt-5-mini".into(),
            }],
        )
        .unwrap();
        assert!(r.swaps[0].approximate_cross_provider, "anthropic -> openai");
        assert!(r.approximate);
    }

    #[test]
    fn recommend_ranks_cheaper_first_and_nothing_is_exact() {
        let runs = opus_run(); // anthropic claude-opus-4-8
                               // Same-provider only: candidates are the anthropic rows; cheaper models rank first.
        let r = recommend(&runs, &pricing(), false);
        assert!(!r.recommendations.is_empty());
        assert!(r.approximate);
        // Sorted cheaper-first (delta ascending).
        for w in r.recommendations.windows(2) {
            assert!(w[0].delta_micros <= w[1].delta_micros);
        }
        // Every recommendation is approximate; none is labeled exact.
        assert!(r.recommendations.iter().all(|x| x.approximate_tokenizer));
        // Same-provider mode never recommends an openai/gemini target.
        assert!(r
            .recommendations
            .iter()
            .all(|x| x.to_provider == "anthropic"));
        // Cross-provider opens the field to cheaper foreign models, all cross-flagged.
        let x = recommend(&runs, &pricing(), true);
        assert!(x
            .recommendations
            .iter()
            .any(|c| c.to_provider != "anthropic"));
        assert!(x
            .recommendations
            .iter()
            .filter(|c| c.to_provider != "anthropic")
            .all(|c| c.approximate_cross_provider));
    }

    #[test]
    fn unpriced_target_hard_errors() {
        let runs = opus_run();
        let err = whatif(
            &runs,
            &pricing(),
            &[Swap::AllTo {
                to: "model-that-does-not-exist".into(),
            }],
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("no bundled price"));
    }
}
