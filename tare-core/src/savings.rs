//! The unified Savings Ledger: ONE ranked list of recoverable-dollar opportunities,
//! normalized from Tare's separate act-side engines so the user sees a single worklist instead of
//! four disconnected screens. Pure, integer micro-USD, deterministic.
//!
//! Sources, each normalized into an [`Opportunity`]:
//!   - retry-loop waste ([`crate::session::loop_waste`]) — dollars spent re-issuing identical calls
//!   - failure waste ([`crate::session::failure_waste`]) — dollars spent on errored/refused steps
//!   - wasted cache writes ([`crate::session::wasted_cache_write`]) — write premium paid, never read
//!   - prompt-cache advice ([`crate::advise::advise`]) — dollars a cache write/read tier would save
//!   - context-bloat ([`crate::session::context_bloat_waste`]) — re-caching an eroded prefix
//!   - model-swap what-if ([`crate::whatif::recommend`]) — dollars the cheapest fleet swap would save
//!   - rightsizing ([`crate::whatif::downshift_waste`]) — simple calls downshifted off a premium model
//!
//! This engine normalizes and ranks opportunities, then computes a spend-bounded capped-potential
//! headline and Savings Index. It does not subtract step-level overlap between opportunity classes.
//! The `confidence` field is honest about how solid each number is: loop/failure are *measured* (the
//! money was already spent on nothing), cache is *projected*, and model-swap is *approximate* (a
//! different model tokenizes differently — the what-if caveat).

use crate::advise::{advise, suboptimal_breakpoint};
use crate::cohort::{
    CohortDimension, CohortEntity, CohortFilter, CohortMetric, CohortSpec, MatchRule,
    Normalization, OutcomeDenominator, PricingMode, StepRef, MAX_RUN_IDS, MAX_STEP_REFS,
};
use crate::model::{Provider, RunRecord};
use crate::pricing::PricingTable;
use crate::session::{context_bloat_waste, failure_waste, loop_waste, wasted_cache_write};
use crate::whatif::{downshift_waste, recommend};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opportunity {
    /// `loop` | `failure` | `wasted-write` | `cache` | `breakpoint` | `context-bloat` |
    /// `model-swap` | `rightsizing` — the source engine / waste class.
    pub kind: String,
    /// Human label (offending tool/agent, session, or provider-qualified model).
    pub label: String,
    /// Recoverable spend in micro-USD if the fix is applied (always >= 0).
    pub recoverable_micros: i64,
    /// How solid the number is: `measured` | `projected` | `approximate`.
    pub confidence: String,
    /// Copy-pasteable remediation.
    pub fix_text: String,
    /// Rough effort to apply: `S` | `M` | `L`.
    pub effort: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsLedger {
    /// Opportunities sorted by `recoverable_micros` desc (deterministic tiebreak by kind then label).
    /// Each row's `recoverable_micros` is a per-category UPPER BOUND (categories can overlap at the
    /// step level — e.g. a model swap also reduces a looped step's cost).
    pub opportunities: Vec<Opportunity>,
    /// **Capped potential savings**: opportunities
    /// are drained in precedence order (measured waste -> projected cache -> approximate swaps)
    /// against a budget seeded from total spend, so this headline can NEVER exceed what was actually
    /// spent (the bug the old explain.rs naive-sum had). Precedence orders categories by certainty,
    /// so when the spend budget binds the most-certain savings are kept. It is **not** a
    /// non-overlapping floor and **not** deduplicated: categories can overlap at the step level, so
    /// this is bounded-by-spend, not exact overlap subtraction (it can double-count overlap while the
    /// budget is ample — but is always ≤ total spend, unlike a naive sum). Until step-level overlap
    /// subtraction lands and is tested, never call this a floor or a deduplicated total.
    /// The serialized field name stays `total_recoverable_micros` for wire compatibility.
    pub total_recoverable_micros: i64,
    /// Total estimated spend across the runs (the denominator for the index).
    pub total_spend_micros: i64,
    /// Savings Index 0..100 (FinOps COIN analogue): 100 = nothing recoverable, lower = more waste.
    /// `100 - capped_potential/spend*100`; 100 when there is no priced spend.
    pub savings_index: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

fn provider_model_label(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

/// Build the unified ledger across all act-side engines. Pure function of stored steps + pricing.
pub fn savings(runs: &[RunRecord], pricing: &PricingTable) -> SavingsLedger {
    let mut opps: Vec<Opportunity> = Vec::new();

    // Retry-loop waste: the redundant spend IS the recoverable amount (capping the loop saves it).
    for r in loop_waste(runs, pricing).rows {
        if r.micros > 0 {
            opps.push(Opportunity {
                kind: "loop".into(),
                label: r.label,
                recoverable_micros: r.micros,
                confidence: "measured".into(),
                fix_text: format!(
                    "Identical request issued up to {}x. Cap it in tare.toml:\n[budget]\nmax_repeats = {}",
                    r.max_repeat,
                    r.max_repeat.saturating_sub(1).max(1)
                ),
                effort: "S".into(),
            });
        }
    }

    // Failure waste: dollars spent on errored/refused steps — recoverable by gating the cause.
    for r in failure_waste(runs, pricing).rows {
        if r.micros > 0 {
            opps.push(Opportunity {
                kind: "failure".into(),
                label: r.label.clone(),
                recoverable_micros: r.micros,
                confidence: "measured".into(),
                fix_text: format!(
                    "{} step(s) on `{}` errored/refused — investigate or gate this tool/agent so it stops retrying into failures.",
                    r.failed_steps, r.label
                ),
                effort: "S".into(),
            });
        }
    }

    // Wasted cache writes: a prefix written to the cache (paying the 1.25×/2× premium)
    // but never read back — the premium bought nothing. Measured: the write happened, the read didn't.
    for r in wasted_cache_write(runs, pricing).rows {
        if r.micros > 0 {
            opps.push(Opportunity {
                kind: "wasted-write".into(),
                label: provider_model_label(&r.provider, &r.model),
                recoverable_micros: r.micros,
                confidence: "measured".into(),
                fix_text: format!(
                    "{} cache-write tokens on `{}` paid the cache-write premium but were never read back in this window. Drop `cache_control` on single-use prefixes, or ensure the prefix is stable + reused before the TTL expires.",
                    r.write_tokens, r.model
                ),
                effort: "S".into(),
            });
        }
    }

    // Prompt-cache advice: the recommended tier's projected saving.
    for a in advise(runs, pricing) {
        let saved = match a.recommend.as_str() {
            "5m" => a.save_5m_micros,
            "1h" => a.save_1h_micros,
            _ => 0,
        };
        if saved > 0 {
            opps.push(Opportunity {
                kind: "cache".into(),
                label: provider_model_label(&a.provider, &a.model),
                recoverable_micros: saved,
                confidence: "projected".into(),
                fix_text: format!(
                    "Mark the stable prefix (system + tools) cacheable for {}. Anthropic: add to that block\n\"cache_control\": {{\"type\": \"ephemeral\"{}}}",
                    a.model,
                    if a.recommend == "1h" { ", \"ttl\": \"1h\"" } else { "" }
                ),
                effort: "M".into(),
            });
        }
    }

    // Suboptimal cache breakpoint: the group DOES cache, but on the `tools` block,
    // leaving the stable `system` prefix re-billed fresh each send. Moving the breakpoint down to
    // `system` caches it too. Projected, like cache — advise() skips these (they set cache_control).
    for b in suboptimal_breakpoint(runs, pricing) {
        let saved = match b.recommend.as_str() {
            "5m" => b.save_5m_micros,
            "1h" => b.save_1h_micros,
            _ => 0,
        };
        if saved > 0 {
            opps.push(Opportunity {
                kind: "breakpoint".into(),
                label: provider_model_label(&b.provider, &b.model),
                recoverable_micros: saved,
                confidence: "projected".into(),
                fix_text: format!(
                    "The cache breakpoint on `{}` is on the tools block, so the stable system prefix ({} tokens) is re-billed fresh across {} sends. Move `cache_control` to the last stable block (system){} so it's cached too.",
                    b.model,
                    b.stranded_tokens,
                    b.sends,
                    if b.recommend == "1h" { " with \"ttl\": \"1h\"" } else { "" }
                ),
                effort: "M".into(),
            });
        }
    }

    // Context-bloat waste: a session whose cached prefix eroded to fresh-input —
    // re-caching the stable prefix recovers the (fresh − cache_read) delta. Projected, like cache.
    for r in context_bloat_waste(runs, pricing).rows {
        if r.micros > 0 {
            opps.push(Opportunity {
                kind: "context-bloat".into(),
                label: r.session.clone(),
                recoverable_micros: r.micros,
                confidence: "projected".into(),
                fix_text: format!(
                    "Session `{}` stopped hitting the prompt cache at turn {} ({} fresh-input tokens re-sent uncached). Re-cache the stable prefix (system + tools + early history) so it's served at the cache-read rate.",
                    r.session, r.erosion_turn, r.fresh_tokens_after
                ),
                effort: "M".into(),
            });
        }
    }

    // Rightsizing: simple/low-output calls on a premium model repriced onto the cheapest
    // same-provider model. Approximate (different tokenizer); residual after the wholesale swap.
    for r in downshift_waste(runs, pricing).rows {
        if r.micros > 0 {
            opps.push(Opportunity {
                kind: "rightsizing".into(),
                label: provider_model_label(&r.provider, &r.from_model),
                recoverable_micros: r.micros,
                confidence: "approximate".into(),
                fix_text: format!(
                    "{} simple call(s) on {}/{} (output < {} tokens, no cache) would cost less on {} — route low-complexity tasks to the smaller model. Estimate reuses the captured token counts; a different tokenizer would shift it.",
                    r.steps, r.provider, r.from_model, crate::whatif::SIMPLE_OUTPUT_MAX, r.to_model
                ),
                effort: "M".into(),
            });
        }
    }

    // Model-swap what-if: the single cheapest same-tokenizer-caveat swap (most negative delta).
    let rec = recommend(runs, pricing, false);
    if let Some(best) = rec
        .recommendations
        .iter()
        .filter(|r| r.delta_micros < 0)
        .min_by_key(|r| r.delta_micros)
    {
        opps.push(Opportunity {
            kind: "model-swap".into(),
            label: provider_model_label(&best.to_provider, &best.to_model),
            recoverable_micros: best.delta_micros.saturating_neg(),
            confidence: "approximate".into(),
            fix_text: format!(
                "Switch to {} (approximate — different tokenizer).",
                best.to_model
            ),
            effort: "M".into(),
        });
    }

    opps.sort_by(|a, b| {
        b.recoverable_micros
            .cmp(&a.recoverable_micros)
            .then(a.kind.cmp(&b.kind))
            .then(a.label.cmp(&b.label))
    });

    // CAPPED POTENTIAL savings (honest rename): drain a budget = total spend
    // in precedence order so the headline can never exceed what was spent. Measured waste claims its
    // dollars first; the approximate model-swap only claims the residual. This is a spend CAP, NOT a
    // non-overlapping floor and NOT deduplicated (see `total_recoverable_micros`).
    let total_spend_micros = crate::lenses::lenses(runs, pricing).total_micros.max(0);
    // Order by CERTAINTY (most-certain first), so when the spend budget binds the firmer savings
    // are the ones kept. Overlapping categories (cache↔context-bloat re-cache the same prefix;
    // model-swap↔rightsizing reprice the same steps) are NOT step-level overlap-subtracted — the
    // capped potential is bounded by spend, not exact (see `total_recoverable_micros`).
    let precedence = |kind: &str| match kind {
        "loop" | "failure" | "wasted-write" => 0, // measured — money already spent on nothing
        "cache" | "breakpoint" => 1,              // projected — (re-)caching the stable prefix
        "context-bloat" => 2, // projected — re-caching an eroded prefix (overlaps cache)
        "model-swap" => 3,    // approximate — wholesale fleet swap
        "rightsizing" => 4,   // approximate — per-step downshift (overlaps model-swap)
        _ => 5,
    };
    let mut drain_order: Vec<&Opportunity> = opps.iter().collect();
    drain_order.sort_by(|a, b| {
        precedence(&a.kind)
            .cmp(&precedence(&b.kind))
            .then(b.recoverable_micros.cmp(&a.recoverable_micros))
    });
    let mut remaining = total_spend_micros;
    let mut capped = 0i64;
    for o in drain_order {
        let take = o.recoverable_micros.min(remaining).max(0);
        capped = capped.saturating_add(take);
        remaining = remaining.saturating_sub(take);
        if remaining <= 0 {
            break;
        }
    }
    let savings_index = if total_spend_micros > 0 {
        let recoverable_pct = i128::from(capped) * 100 / i128::from(total_spend_micros);
        i64::try_from((100i128 - recoverable_pct).clamp(0, 100)).unwrap_or(0)
    } else {
        100
    };

    SavingsLedger {
        opportunities: opps,
        total_recoverable_micros: capped,
        total_spend_micros,
        savings_index,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

// ---- Opportunity v2: stable identity + evidence ------------------------

/// Detector version for stable opportunity identity. Bump ONLY when identity semantics
/// intentionally change — a bump re-keys every opportunity, which resets any saved lifecycle state.
pub const DETECTOR_VERSION: &str = "2";

/// Max inline evidence references per opportunity. Full counts are always preserved; the
/// cohort snapshot uses semantic filters when possible and otherwise the cohort API's explicit-id
/// bounds.
pub const EVIDENCE_CAP: usize = 500;

/// A specific step an opportunity's evidence points at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpportunityStepRef {
    pub run_id: String,
    pub step_ordinal: u32,
}

/// Stable, detector-versioned opportunity key: `{kind}:{fnv1a_64(version \0 kind \0 target):016x}`.
/// `canonical_target` is the detector-owned STABLE identity (a tool/agent/model/session id) — NEVER
/// the mutable display sentence — so rewording `fix_text` or reformatting the label never re-keys it.
pub fn opportunity_key(kind: &str, canonical_target: &str) -> String {
    let material = format!("{DETECTOR_VERSION}\u{0}{kind}\u{0}{canonical_target}");
    format!(
        "{kind}:{:016x}",
        crate::canon::fnv1a_64(material.as_bytes())
    )
}

/// v2 opportunity: the v1 fields plus a stable key, capped evidence refs + full counts, the
/// exact cohort snapshot, assumptions, and an optional quality risk. Additive — v1 consumers ignore
/// the new fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpportunityV2 {
    pub opportunity_key: String,
    pub kind: String,
    pub label: String,
    pub recoverable_micros: i64,
    pub confidence: String,
    pub fix_text: String,
    pub effort: String,
    pub affected_run_count: u64,
    pub affected_step_count: u64,
    pub affected_run_ids: Vec<String>,
    pub affected_steps: Vec<OpportunityStepRef>,
    pub evidence_truncated: bool,
    pub evidence_method: String,
    pub cohort_snapshot: CohortSpec,
    pub assumptions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_risk: Option<String>,
}

/// The additive v2 ledger: every v1 field retained (old consumers keep working) plus the
/// v2 rows and the explicit capped-potential / applied / observed totals. Store-backed producers
/// populate the lifecycle totals with [`savings_v2_with_lifecycle`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsLedgerV2 {
    pub opportunities: Vec<Opportunity>,
    pub total_recoverable_micros: i64,
    pub total_spend_micros: i64,
    pub savings_index: i64,
    pub pricing_version: String,
    pub estimated: bool,
    pub opportunities_v2: Vec<OpportunityV2>,
    /// Explicit alias of `total_recoverable_micros` under an honest name: CAPPED POTENTIAL (bounded
    /// by spend), NOT a deduplicated non-overlapping floor — categories can overlap at the step level.
    pub capped_potential_micros: i64,
    /// Expected-point exposure on persisted applied actions. Action cohorts can overlap, so this is
    /// not a deduplicated saving and must never be added to capped potential.
    pub applied_micros: i64,
    /// Positive, complete action-local observed reductions. Action cohorts can overlap, so this is
    /// not deduplicated and must never be added to capped potential.
    pub observed_micros: i64,
}

/// Live action-lifecycle totals supplied by the store. Keeping these separate from opportunity
/// detection preserves the pure core calculation while making the exposure/observation boundary
/// explicit: neither value changes capped potential, and neither value is overlap-deduplicated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsLifecycleTotals {
    pub applied_micros: i64,
    pub observed_micros: i64,
}

/// The identity axis an opportunity's evidence scans by (derived from its kind).
enum EvidenceAxis {
    ProviderModel,
    Session,
    ToolAgent,
    Fleet,
}

fn evidence_axis(kind: &str) -> EvidenceAxis {
    match kind {
        "cache" | "breakpoint" | "wasted-write" | "rightsizing" => EvidenceAxis::ProviderModel,
        "context-bloat" => EvidenceAxis::Session,
        "loop" | "failure" => EvidenceAxis::ToolAgent,
        // model-swap's target is a DESTINATION model absent from the data → it applies fleet-wide.
        _ => EvidenceAxis::Fleet,
    }
}

fn step_on_axis(axis: &EvidenceAxis, target: &str, step: &crate::model::StepRecord) -> bool {
    match axis {
        EvidenceAxis::ProviderModel => {
            let provider = step.provider.pricing_key(step.shape.vendor.as_deref());
            target == provider_model_label(provider.as_ref(), &step.model)
        }
        EvidenceAxis::Session => step.shape.session.as_deref() == Some(target),
        EvidenceAxis::ToolAgent => [
            &step.shape.component_label,
            &step.shape.parent_label,
            &step.shape.step_label,
        ]
        .iter()
        .any(|l| l.as_deref() == Some(target)),
        EvidenceAxis::Fleet => true,
    }
}

/// Enrich one v1 [`Opportunity`] into an [`OpportunityV2`]: derive the affected run/step evidence by
/// scanning the runs on the opportunity's identity axis, build a resolvable cohort snapshot, and
/// attach honest assumptions + quality risk. Inline evidence is capped at [`EVIDENCE_CAP`]; full
/// counts are retained, while explicit snapshot ids/refs obey the cohort contract's larger bounds.
fn enrich(o: &Opportunity, runs: &[RunRecord]) -> OpportunityV2 {
    let axis = evidence_axis(&o.kind);
    let target = o.label.as_str(); // the detector-owned identity today (never the fix_text sentence)

    let mut refs: Vec<OpportunityStepRef> = Vec::new();
    let mut run_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for run in runs {
        for step in &run.steps {
            if step_on_axis(&axis, target, step) {
                refs.push(OpportunityStepRef {
                    run_id: run.run_id.clone(),
                    step_ordinal: step.step_ordinal,
                });
                run_set.insert(run.run_id.clone());
            }
        }
    }
    refs.sort_by(|a, b| {
        a.run_id
            .cmp(&b.run_id)
            .then(a.step_ordinal.cmp(&b.step_ordinal))
    });
    let run_ids: Vec<String> = run_set.into_iter().collect(); // BTreeSet → already sorted
    let affected_run_count = u64::try_from(run_ids.len()).unwrap_or(u64::MAX);
    let affected_step_count = u64::try_from(refs.len()).unwrap_or(u64::MAX);
    let evidence_truncated = refs.len() > EVIDENCE_CAP || run_ids.len() > EVIDENCE_CAP;

    // Resolvable cohort snapshot for the affected set: semantic filters where dimensions are
    // unambiguous, otherwise explicit ids/refs bounded by the cohort contract.
    let (filters, evidence_method) = match axis {
        EvidenceAxis::ProviderModel => {
            let (provider, model) = target.split_once('/').unwrap_or(("", target));
            if Provider::parse(provider).is_some() {
                (
                    vec![
                        CohortFilter::Eq {
                            dimension: CohortDimension::Provider,
                            value: provider.to_string(),
                        },
                        CohortFilter::Eq {
                            dimension: CohortDimension::Model,
                            value: model.to_string(),
                        },
                    ],
                    format!("steps priced on provider/model `{target}`"),
                )
            } else {
                (
                    vec![CohortFilter::StepRefs {
                        refs: refs
                            .iter()
                            .take(MAX_STEP_REFS)
                            .map(|r| StepRef {
                                run_id: r.run_id.clone(),
                                step_ordinal: r.step_ordinal,
                            })
                            .collect(),
                    }],
                    format!("steps priced by compatible vendor/model `{target}`"),
                )
            }
        }
        EvidenceAxis::Session => (
            vec![CohortFilter::Eq {
                dimension: CohortDimension::Session,
                value: target.to_string(),
            }],
            format!("steps in session `{target}`"),
        ),
        EvidenceAxis::ToolAgent => (
            vec![CohortFilter::RunIds {
                ids: run_ids.iter().take(MAX_RUN_IDS).cloned().collect(),
            }],
            format!("steps whose tool/agent label == `{target}`"),
        ),
        EvidenceAxis::Fleet => (
            Vec::new(),
            "fleet-wide model-swap recommendation across all priced runs".to_string(),
        ),
    };
    let cohort_snapshot = CohortSpec {
        from: None,
        to: None,
        timezone: "UTC".to_string(),
        entity: CohortEntity::Run,
        filters,
        pricing: PricingMode::EffectiveDated,
        metric: CohortMetric::SpendMicros,
        normalization: Normalization::Absolute,
        outcome_denominator: None,
    };

    let assumptions = match o.confidence.as_str() {
        "measured" => vec!["measured: this spend was already incurred on wasted work".to_string()],
        "projected" => vec![
            "projected: assumes the stable prefix stays cacheable and is reused within its TTL"
                .to_string(),
        ],
        _ => vec![
            "approximate: a different model tokenizes differently, so the recount may shift the estimate"
                .to_string(),
        ],
    };
    // Only model-changing recommendations carry a quality risk; measured/projected waste does not.
    let quality_risk = (o.confidence == "approximate").then(|| {
        "Swapping models can change output quality — verify on a representative sample before rolling out."
            .to_string()
    });

    OpportunityV2 {
        opportunity_key: opportunity_key(&o.kind, target),
        kind: o.kind.clone(),
        label: o.label.clone(),
        recoverable_micros: o.recoverable_micros,
        confidence: o.confidence.clone(),
        fix_text: o.fix_text.clone(),
        effort: o.effort.clone(),
        affected_run_count,
        affected_step_count,
        affected_run_ids: run_ids.into_iter().take(EVIDENCE_CAP).collect(),
        affected_steps: {
            refs.truncate(EVIDENCE_CAP);
            refs
        },
        evidence_truncated,
        evidence_method,
        cohort_snapshot,
        assumptions,
        quality_risk,
    }
}

/// Build the additive v2 ledger with live store-backed action totals. The lifecycle values are
/// deliberately copied into their own rows rather than combined with capped potential: applied is
/// an expected-point exposure, observed is a verification result, and action cohorts may overlap.
pub fn savings_v2_with_lifecycle(
    runs: &[RunRecord],
    pricing: &PricingTable,
    lifecycle: SavingsLifecycleTotals,
) -> SavingsLedgerV2 {
    let v1 = savings(runs, pricing);
    let opportunities_v2 = v1.opportunities.iter().map(|o| enrich(o, runs)).collect();
    SavingsLedgerV2 {
        opportunities: v1.opportunities,
        total_recoverable_micros: v1.total_recoverable_micros,
        total_spend_micros: v1.total_spend_micros,
        savings_index: v1.savings_index,
        pricing_version: v1.pricing_version,
        estimated: v1.estimated,
        opportunities_v2,
        capped_potential_micros: v1.total_recoverable_micros,
        applied_micros: lifecycle.applied_micros,
        observed_micros: lifecycle.observed_micros,
    }
}

// ---- Savings action lifecycle ------------------------------------------

/// Baseline rule persisted with a savings action (`BaselineDto`) — the snake_case wire form of the
/// UI `BaselineSpec`. Carried so the intervention can be re-resolved and verified later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineDto {
    pub kind: String,
    pub label: String,
    pub cohort: CohortSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_count: Option<u32>,
}

/// An apply/dismiss request: everything needed to later VERIFY the intervention — the exact
/// cohort, baseline rule, match rule, metric/normalization/outcome, expected range, quality guardrail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsActionRequest {
    pub opportunity_key: String,
    pub cohort: CohortSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineDto>,
    /// `match` on the wire (a Rust keyword), matching `SavingsActionRequest.match`.
    #[serde(rename = "match")]
    pub match_rule: MatchRule,
    pub metric: CohortMetric,
    pub normalization: Normalization,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_denominator: Option<OutcomeDenominator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_low_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_point_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_high_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_guardrail: Option<i64>,
}

/// Identity of a persisted action: the opportunity + the cohort hash it was taken against.
/// Unaccept uses this to remove the exact row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsActionIdentity {
    pub opportunity_key: String,
    pub cohort_hash: String,
}

/// A stored savings action as read back: the persisted row plus computed
/// `compatibility_warnings`. `aggregate_only` matches (including legacy-migrated acceptances) always
/// carry a confounding warning — Tare never claims a causal saving without a match rule to justify it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsAction {
    pub opportunity_key: String,
    pub cohort_hash: String,
    /// `applied` | `dismissed` (the only persisted statuses; verification states are derived).
    pub status: String,
    pub acted_at: String,
    pub cohort: CohortSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineDto>,
    #[serde(rename = "match")]
    pub match_rule: MatchRule,
    pub metric: CohortMetric,
    pub normalization: Normalization,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_denominator: Option<OutcomeDenominator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_low_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_point_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_high_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_guardrail: Option<i64>,
    pub compatibility_warnings: Vec<String>,
}

impl SavingsAction {
    /// Honest compatibility warnings for a stored action. An `aggregate_only` match —
    /// which includes every legacy-migrated acceptance — can never justify a causal saving, so it
    /// always warns; the verification path adds window-specific warnings on top.
    pub fn warnings_for(match_rule: &MatchRule) -> Vec<String> {
        match match_rule {
            MatchRule::AggregateOnly => vec![
                "aggregate-only action — observed association, not a causal saving; before/after units are not matched".to_string(),
            ],
            _ => Vec::new(),
        }
    }
}

/// A verification request: identity + an optional as-of date and window size. Extends the
/// action identity — verification re-resolves the STORED cohort/baseline/match over before/after
/// windows around the action's `acted_at`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsVerifyRequest {
    pub opportunity_key: String,
    pub cohort_hash: String,
    /// The "now" date for completeness (RFC3339 date `YYYY-MM-DD`); defaults to the latest captured
    /// day so completeness is clock-free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_date: Option<String>,
    /// Equal before/after window size in calendar days (default 7). The intervention day is excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_days: Option<u32>,
}

/// The result of cohort-scoped OBSERVED-REDUCTION verification. Deliberately NOT the
/// global `realization()` result: equal local-date windows exclude the intervention day, the
/// stored scope/baseline/match are re-resolved, and the wording stays "observed reduction" — never a
/// causal "realized saving" — unless a real match rule + denominator justify it. `status` is
/// `verifying` (incomplete after window) | `observed_reduction` (complete + positive) | `not_observed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsVerifyResult {
    pub status: String,
    pub complete: bool,
    pub selection_before_micros: i64,
    pub selection_after_micros: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_before_micros: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_after_micros: Option<i64>,
    /// With a baseline: `(selection_before - selection_after) + (baseline_after - baseline_before)`
    /// (difference-in-differences). Without one: the unadjusted selection reduction (carries a
    /// warning that it may reflect an overall trend).
    pub observed_reduction_micros: i64,
    pub matched_before: u64,
    pub matched_after: u64,
    pub unmatched_before: u64,
    pub unmatched_after: u64,
    /// Baseline match/exclusion counts are present only when the stored action has a baseline. They
    /// make the full like-for-like intersection traceable instead of hiding baseline exclusions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_matched_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_matched_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_unmatched_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_unmatched_after: Option<u64>,
    pub compatibility_warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, StepMeta};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    // Two identical OpenAI calls in a run -> the 2nd is redundant loop waste -> a 'loop' opportunity.
    fn looping_run() -> Vec<RunRecord> {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let step = |ord: u32| {
            crate::ingest_step_ct(
                "r",
                ord,
                Provider::Openai,
                req,
                resp,
                None,
                &crate::PrivacyPolicy::default(),
                &StepMeta {
                    workload_key: None,
                    component_label: Some("search".into()),
                    ..Default::default()
                },
                None,
                0,
            )
            .unwrap()
        };
        vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(1), step(2)],
        }]
    }

    /// A session whose cached prefix erodes: turn 1 reads cache, turn 2 re-sends it as fresh input.
    fn eroded_session() -> Vec<RunRecord> {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let mk = |ord: u32, fresh: u64, cread: u64| {
            let mut s = crate::ingest_step_ct(
                "r",
                ord,
                Provider::Openai,
                req,
                resp,
                None,
                &crate::PrivacyPolicy::default(),
                &StepMeta::default(),
                None,
                0,
            )
            .unwrap();
            s.usage.fresh_input = fresh;
            s.usage.cache_read = cread;
            s.shape.session = Some("sess".into());
            s
        };
        vec![RunRecord {
            run_id: "r".into(),
            steps: vec![mk(1, 100, 500), mk(2, 800, 0)],
        }]
    }

    #[test]
    fn opportunity_key_is_stable_identity_not_display() {
        // The key derives from (detector_version, kind, canonical_target) only.
        let k = opportunity_key("loop", "search");
        assert!(k.starts_with("loop:"), "kind-prefixed: {k}");
        assert_eq!(k, opportunity_key("loop", "search")); // deterministic
        assert_ne!(k, opportunity_key("loop", "other")); // target changes the key
        assert_ne!(k, opportunity_key("failure", "search")); // kind changes the key
                                                             // Keys survive display (fix_text) changes: same (kind,target), different sentence → same key.
        let opp = |fix: &str| Opportunity {
            kind: "cache".into(),
            label: "openai/m".into(),
            recoverable_micros: 1,
            confidence: "projected".into(),
            fix_text: fix.into(),
            effort: "S".into(),
        };
        assert_eq!(
            enrich(&opp("do X"), &[]).opportunity_key,
            enrich(&opp("do Y — totally reworded"), &[]).opportunity_key
        );
    }

    #[test]
    fn evidence_truncation_preserves_counts_and_cohort() {
        // Cap the inline list at 500 while preserving the full count and semantic snapshot.
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let base = crate::ingest_step_ct(
            "r",
            1,
            Provider::Openai,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            &StepMeta::default(),
            None,
            0,
        )
        .unwrap();
        let steps: Vec<crate::model::StepRecord> = (0..501)
            .map(|i| {
                let mut s = base.clone();
                s.step_ordinal = i;
                s.model = "m".into();
                s.shape.model = "m".into();
                s
            })
            .collect();
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps,
        }];
        let opp = Opportunity {
            kind: "cache".into(),
            label: "openai/m".into(),
            recoverable_micros: 10,
            confidence: "projected".into(),
            fix_text: "cache it".into(),
            effort: "S".into(),
        };
        let v2 = enrich(&opp, &runs);
        assert_eq!(v2.affected_step_count, 501); // full count preserved
        assert_eq!(v2.affected_steps.len(), EVIDENCE_CAP); // inline list capped
        assert!(v2.evidence_truncated);
        // Provider + model keep the semantic snapshot unambiguous and untruncated.
        assert_eq!(
            v2.cohort_snapshot.filters,
            vec![
                CohortFilter::Eq {
                    dimension: CohortDimension::Provider,
                    value: "openai".into()
                },
                CohortFilter::Eq {
                    dimension: CohortDimension::Model,
                    value: "m".into()
                }
            ]
        );
        assert!(v2.quality_risk.is_none()); // projected caching has no model-swap quality risk
    }

    #[test]
    fn provider_qualified_model_evidence_does_not_cross_backends() {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let mut openai = crate::ingest_step_ct(
            "openai-run",
            1,
            Provider::Openai,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            &StepMeta::default(),
            None,
            0,
        )
        .unwrap();
        openai.model = "shared-name".into();
        openai.shape.model = "shared-name".into();
        let mut anthropic = openai.clone();
        anthropic.run_id = "anthropic-run".into();
        anthropic.provider = Provider::Anthropic;
        anthropic.shape.provider = Provider::Anthropic;
        let runs = vec![
            RunRecord {
                run_id: "openai-run".into(),
                steps: vec![openai],
            },
            RunRecord {
                run_id: "anthropic-run".into(),
                steps: vec![anthropic],
            },
        ];
        let opportunity = Opportunity {
            kind: "cache".into(),
            label: "openai/shared-name".into(),
            recoverable_micros: 1,
            confidence: "projected".into(),
            fix_text: "cache it".into(),
            effort: "S".into(),
        };
        let enriched = enrich(&opportunity, &runs);
        assert_eq!(enriched.affected_run_ids, vec!["openai-run"]);
        assert_eq!(enriched.affected_step_count, 1);
        assert_ne!(
            enriched.opportunity_key,
            opportunity_key("cache", "anthropic/shared-name")
        );
    }

    #[test]
    fn savings_v2_is_additive_and_does_not_dedupe_overlap() {
        // Compatibility fields are retained; capped potential is the spend-bounded headline (not a
        // deduped floor); every v1 opportunity has a v2 row (no overlap collapse); lifecycle totals
        // remain separate and are copied without changing the potential calculation.
        let lifecycle = SavingsLifecycleTotals {
            applied_micros: 12_300,
            observed_micros: 4_500,
        };
        let v2 = savings_v2_with_lifecycle(&looping_run(), &pricing(), lifecycle);
        assert_eq!(v2.capped_potential_micros, v2.total_recoverable_micros);
        assert_eq!(v2.opportunities_v2.len(), v2.opportunities.len());
        assert_eq!(v2.applied_micros, 12_300);
        assert_eq!(v2.observed_micros, 4_500);
        let loop_opp = v2
            .opportunities_v2
            .iter()
            .find(|o| o.kind == "loop")
            .expect("a loop opportunity");
        assert!(loop_opp.opportunity_key.starts_with("loop:"));
        assert!(loop_opp.affected_step_count >= 1); // the "search" tool steps are real evidence
        assert!(loop_opp.evidence_method.contains("tool/agent"));
        assert_eq!(loop_opp.confidence, "measured");
    }

    /// A single small, cache-less call on premium opus — a textbook rightsizing candidate.
    fn simple_premium_run() -> Vec<RunRecord> {
        let req = include_bytes!("../../fixtures/anthropic_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_nonstream/response.json");
        let mut s = crate::ingest_step_ct(
            "r",
            1,
            Provider::Anthropic,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            &StepMeta::default(),
            None,
            0,
        )
        .unwrap();
        s.usage.output = 100;
        s.usage.fresh_input = 2000;
        s.usage.cache_read = 0;
        s.usage.cache_write_5m = 0;
        s.usage.cache_write_1h = 0;
        vec![RunRecord {
            run_id: "r".into(),
            steps: vec![s],
        }]
    }

    /// A prefix written to cache but never read back — the write premium bought nothing.
    fn wasted_write_run() -> (Vec<RunRecord>, PricingTable) {
        let pr = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000}]}"#,
        )
        .unwrap();
        let step = crate::model::StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: crate::model::UsageTokens {
                cache_write_5m: 1_000_000,
                ..Default::default()
            },
            shape: crate::model::RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: true,
                cached_component: None,
                system_hash: Some(1),
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
        (
            vec![RunRecord {
                run_id: "r".into(),
                steps: vec![step],
            }],
            pr,
        )
    }

    #[test]
    fn wasted_cache_write_surfaces_as_a_measured_opportunity() {
        let (runs, pr) = wasted_write_run();
        let led = savings(&runs, &pr);
        let w = led
            .opportunities
            .iter()
            .find(|o| o.kind == "wasted-write")
            .expect("a write-without-read prefix -> wasted-write opportunity");
        assert_eq!(w.confidence, "measured");
        assert_eq!(w.recoverable_micros, 750_000, "(3.75 − 3.00) × 1M tok");
        assert!(w.fix_text.contains("cache_control"));
        assert!(led.total_recoverable_micros <= led.total_spend_micros);
    }

    #[test]
    fn rightsizing_surfaces_in_the_ledger() {
        let led = savings(&simple_premium_run(), &pricing());
        let r = led
            .opportunities
            .iter()
            .find(|o| o.kind == "rightsizing")
            .expect("a simple premium call -> rightsizing opportunity");
        assert_eq!(r.confidence, "approximate");
        assert!(r.recoverable_micros > 0);
        assert!(r.fix_text.contains("route low-complexity"));
        assert!(led.total_recoverable_micros <= led.total_spend_micros);
    }

    #[test]
    fn breakpoint_surfaces_in_the_ledger() {
        // a group that caches the tools block but re-bills the system prefix fresh.
        let mk = |ord: u32| crate::model::StepRecord {
            run_id: "r".into(),
            step_ordinal: ord,
            provider: crate::model::Provider::Anthropic,
            model: "claude-opus-4-8".into(),
            usage: crate::model::UsageTokens {
                fresh_input: 4000,
                cache_read: 2000,
                ..Default::default()
            },
            shape: crate::model::RequestShape {
                model: "claude-opus-4-8".into(),
                provider: crate::model::Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: true,
                cached_component: Some(crate::model::Component::Tools),
                system_hash: Some(7),
                weights: vec![crate::model::ComponentWeight {
                    component: crate::model::Component::System,
                    bytes: 1,
                }],
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
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![mk(1), mk(2), mk(3)],
        }];
        let led = savings(&runs, &pricing());
        let bp = led
            .opportunities
            .iter()
            .find(|o| o.kind == "breakpoint")
            .expect("too-early breakpoint -> breakpoint opportunity");
        assert_eq!(bp.confidence, "projected");
        assert!(bp.recoverable_micros > 0);
        assert!(
            bp.fix_text.contains("tools block"),
            "fix names the misplacement"
        );
        assert!(led.total_recoverable_micros <= led.total_spend_micros);
    }

    #[test]
    fn context_bloat_surfaces_in_the_ledger_without_double_counting() {
        let led = savings(&eroded_session(), &pricing());
        let cb = led
            .opportunities
            .iter()
            .find(|o| o.kind == "context-bloat")
            .expect("eroded cache -> context-bloat opportunity");
        assert_eq!(cb.confidence, "projected");
        assert!(cb.recoverable_micros > 0);
        assert!(cb.fix_text.contains("turn 2"), "fix names the erosion turn");
        // The capped potential never exceeds total spend (bounded by spend, not a dedup floor).
        assert!(led.total_recoverable_micros <= led.total_spend_micros);
    }

    #[test]
    fn ledger_normalizes_and_ranks_opportunities() {
        let led = savings(&looping_run(), &pricing());
        // The redundant 2nd identical call surfaces as a measured 'loop' opportunity.
        let loop_opp = led.opportunities.iter().find(|o| o.kind == "loop");
        assert!(
            loop_opp.is_some(),
            "redundant identical call -> loop opportunity"
        );
        assert_eq!(loop_opp.unwrap().confidence, "measured");
        assert!(loop_opp.unwrap().recoverable_micros > 0);
        for w in led.opportunities.windows(2) {
            assert!(
                w[0].recoverable_micros >= w[1].recoverable_micros,
                "ranked desc"
            );
        }
        // Capped potential: a spend CAP, never exceeds total spend or the naive sum. It
        // is NOT a non-overlapping floor — categories can overlap (honest labels).
        let naive: i64 = led.opportunities.iter().map(|o| o.recoverable_micros).sum();
        assert!(
            led.total_recoverable_micros <= led.total_spend_micros,
            "capped <= spend"
        );
        assert!(led.total_recoverable_micros <= naive, "capped <= naive sum");
        assert!((0..=100).contains(&led.savings_index));
        assert!(led.total_spend_micros > 0);
    }

    #[test]
    fn loop_opportunity_carries_a_copy_pasteable_budget_snippet() {
        let led = savings(&looping_run(), &pricing());
        let lo = led.opportunities.iter().find(|o| o.kind == "loop").unwrap();
        // The remediation is a ready tare.toml budget cap the user can paste.
        assert!(lo.fix_text.contains("[budget]"));
        assert!(lo.fix_text.contains("max_repeats ="));
    }

    #[test]
    fn empty_runs_yield_an_empty_ledger() {
        let led = savings(&[], &pricing());
        assert!(led.opportunities.is_empty());
        assert_eq!(led.total_recoverable_micros, 0);
        assert_eq!(led.total_spend_micros, 0);
        assert_eq!(led.savings_index, 100, "no spend -> nothing to recover");
        assert!(led.estimated);
    }

    #[test]
    fn savings_index_uses_wide_math_at_i64_limits() {
        let pricing = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"openai","model_id":"m","input_micro_per_mtok":9223372036854775807,
               "output_micro_per_mtok":9223372036854775807,"cache_read_micro_per_mtok":9223372036854775807,
               "cache_write_5m_micro_per_mtok":9223372036854775807,
               "cache_write_1h_micro_per_mtok":9223372036854775807}]}"#,
        )
        .unwrap();
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let mut step = crate::ingest_step_ct(
            "r",
            1,
            Provider::Openai,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            &StepMeta::default(),
            None,
            0,
        )
        .unwrap();
        step.model = "m".into();
        step.shape.model = "m".into();
        step.usage = crate::model::UsageTokens {
            output: u64::MAX,
            ..Default::default()
        };
        step.stop_reason = Some("provider_error".into());
        let ledger = savings(
            &[RunRecord {
                run_id: "r".into(),
                steps: vec![step],
            }],
            &pricing,
        );
        assert_eq!(ledger.total_spend_micros, i64::MAX);
        assert_eq!(ledger.total_recoverable_micros, i64::MAX);
        assert_eq!(ledger.savings_index, 0);
    }
}
