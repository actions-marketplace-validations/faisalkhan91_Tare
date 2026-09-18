//! The unified Action Plan: ONE ranked worklist answering "what should I do, and in
//! what order, by dollars?" It merges the two honesty-distinct engines Tare already computes:
//!   - RECOVERABLE opportunities ([`crate::savings`]) — dollars an applied fix would save, with a
//!     capped-potential headline bounded by actual spend (categories can overlap).
//!   - AT-RISK advisories ([`crate::advisory`]) — dollars *exposed* to a pattern (batch-eligible
//!     spend, disproportionate reasoning / thinking-budget overrun, compressible bulk), which have
//!     NO honest recoverable floor and so are never summed into recoverable.
//!
//! Both are surfaced in one ranked list so the user has a single worklist, but each item is tagged
//! with its `basis` (`recoverable` vs `at-risk`) and the two totals are kept SEPARATE — the
//! capped potential stays bounded (estimate-honesty #6: never present an at-risk upper bound as a
//! saving). Every item carries a dollar figure by construction: pure-$0 diagnostics (the cache
//! scorecard) are dropped — "no recommendation without a dollar figure". Pure,
//! integer micro-USD, deterministic.

use crate::advisory::advisories;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use crate::savings::savings;
use serde::{Deserialize, Serialize};

/// One ranked, dollar-quantified thing to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    /// Source waste class / advisory kind (loop, cache, rightsizing, batch, reasoning-effort, ...).
    pub kind: String,
    /// Model id, prefix, session, or tool the item is about.
    pub label: String,
    /// The dollar figure that ranks this item (always > 0). Its meaning depends on `basis`.
    pub dollars_micros: i64,
    /// `recoverable` — dollars a fix could save (from the savings engines); or `at-risk` —
    /// dollars merely *exposed* to a pattern (an upper bound, never a guaranteed saving).
    pub basis: String,
    /// For recoverable items: `measured` | `projected` | `approximate`. For at-risk: `upper-bound`.
    pub confidence: String,
    /// One-line, ideally copy-pasteable remediation.
    pub action: String,
    /// Rough effort to apply: `S` | `M` | `L`.
    pub effort: String,
}

/// The full ranked worklist plus the two totals kept deliberately separate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionPlan {
    /// Every recoverable opportunity + every $-quantified advisory, ranked by `dollars_micros` desc.
    pub items: Vec<ActionItem>,
    /// Legacy-named capped potential (≤ total spend) from [`crate::savings`]. Categories can overlap;
    /// this is not a deduplicated floor. It never includes at-risk exposure.
    pub recoverable_floor_micros: i64,
    /// Sum of the at-risk upper bounds (overlapping, uncapped) — shown separately, never added to the
    /// capped potential.
    pub at_risk_total_micros: i64,
    pub total_spend_micros: i64,
    /// Savings Index 0..100 from capped potential (carried through from the savings ledger).
    pub savings_index: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Rough effort to apply an advisory's lever (savings opportunities carry their own `effort`).
fn advisory_effort(kind: &str) -> &'static str {
    match kind {
        "reasoning-effort" => "S", // lower the effort tier for this workload
        "batch" => "L",            // re-architect toward the async Batch API
        "compression" => "L",      // needs an external compressor
        _ => "M",
    }
}

fn basis_rank(basis: &str) -> u8 {
    match basis {
        "recoverable" => 0,
        _ => 1,
    }
}

/// Build the unified action plan. Pure function of stored steps + pricing; deterministic.
pub fn action_plan(runs: &[RunRecord], pricing: &PricingTable) -> ActionPlan {
    let ledger = savings(runs, pricing);
    let mut items: Vec<ActionItem> = Vec::new();

    // Recoverable opportunities — already $-quantified (> 0) with confidence + a fix.
    for o in &ledger.opportunities {
        items.push(ActionItem {
            kind: o.kind.clone(),
            label: o.label.clone(),
            dollars_micros: o.recoverable_micros,
            basis: "recoverable".into(),
            confidence: o.confidence.clone(),
            action: o.fix_text.clone(),
            effort: o.effort.clone(),
        });
    }

    // At-risk advisories with a dollar figure. The cache-scorecard advisories are pure diagnostics
    // (at_risk == 0) — dropped, because "no recommendation without a dollar figure".
    let mut at_risk_total = 0i64;
    for a in advisories(runs, pricing) {
        if a.at_risk_micros <= 0 {
            continue;
        }
        at_risk_total = at_risk_total.saturating_add(a.at_risk_micros);
        items.push(ActionItem {
            kind: a.kind.clone(),
            label: a.label.clone(),
            dollars_micros: a.at_risk_micros,
            basis: "at-risk".into(),
            confidence: "upper-bound".into(),
            action: a.headline.clone(),
            effort: advisory_effort(&a.kind).into(),
        });
    }

    // Rank by dollars desc; recoverable before at-risk on ties (a solid saving outranks an exposure);
    // then kind, then label — fully deterministic.
    items.sort_by(|a, b| {
        b.dollars_micros
            .cmp(&a.dollars_micros)
            .then_with(|| basis_rank(&a.basis).cmp(&basis_rank(&b.basis)))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.label.cmp(&b.label))
    });

    ActionPlan {
        items,
        recoverable_floor_micros: ledger.total_recoverable_micros,
        at_risk_total_micros: at_risk_total,
        total_spend_micros: ledger.total_spend_micros,
        savings_index: ledger.savings_index,
        pricing_version: ledger.pricing_version,
        estimated: ledger.estimated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheTtl, Provider, RequestShape, RunRecord, StepRecord, UsageTokens};

    fn flat_pricing() -> PricingTable {
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":5000000,
               "output_micro_per_mtok":25000000,"cache_read_micro_per_mtok":500000,
               "cache_write_5m_micro_per_mtok":6250000,"cache_write_1h_micro_per_mtok":10000000}]}"#,
        )
        .unwrap()
    }

    fn shape() -> RequestShape {
        RequestShape {
            model: "m".into(),
            provider: Provider::Anthropic,
            stream: true,
            ttl: CacheTtl::FiveMin,
            has_cache_control: false,
            cached_component: None,
            system_hash: None,
            weights: vec![],
            request_hash: None,
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
        }
    }

    fn step(ord: u32, usage: UsageTokens, shape: RequestShape) -> StepRecord {
        StepRecord {
            run_id: "r".into(),
            step_ordinal: ord,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage,
            shape,
            stop_reason: Some("end_turn".into()),
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        }
    }

    /// A run that produces BOTH a recoverable opportunity (a wasted cache write) and at-risk
    /// advisories (a non-streaming batch-eligible call), plus a pure-$0 diagnostic (below-minimum
    /// cache_control) that must be dropped.
    fn mixed_run() -> Vec<RunRecord> {
        // Wasted write: 1M cache-write tokens, never read -> recoverable ((6.25-5.0)×1M = 1_250_000).
        let mut wasted = shape();
        wasted.has_cache_control = true;
        wasted.system_hash = Some(1);
        wasted.request_hash = Some(0);
        let wasted_step = step(
            1,
            UsageTokens {
                cache_write_5m: 1_000_000,
                ..Default::default()
            },
            wasted,
        );
        // Non-streaming call -> batch advisory (at-risk). fresh 1000 + output 1000 = 30000µ$, batch 15000.
        let mut batch = shape();
        batch.stream = false;
        let batch_step = step(
            2,
            UsageTokens {
                fresh_input: 1000,
                output: 1000,
                ..Default::default()
            },
            batch,
        );
        // cache_control set, no activity -> below-minimum scorecard advisory (at_risk = 0 -> dropped).
        let mut diag = shape();
        diag.has_cache_control = true;
        diag.system_hash = Some(9);
        let diag_step = step(3, UsageTokens::default(), diag);
        vec![RunRecord {
            run_id: "r".into(),
            steps: vec![wasted_step, batch_step, diag_step],
        }]
    }

    #[test]
    fn merges_recoverable_and_at_risk_into_one_ranked_list() {
        let plan = action_plan(&mixed_run(), &flat_pricing());
        assert!(plan.items.iter().any(|i| i.basis == "recoverable"));
        assert!(plan.items.iter().any(|i| i.basis == "at-risk"));
        // Every item carries a dollar figure (> 0) — "no recommendation without a dollar figure".
        assert!(plan.items.iter().all(|i| i.dollars_micros > 0));
        // Ranked by dollars desc.
        for w in plan.items.windows(2) {
            assert!(w[0].dollars_micros >= w[1].dollars_micros);
        }
        // At-risk items are labelled upper-bound, not a confidence tier.
        assert!(plan
            .items
            .iter()
            .filter(|i| i.basis == "at-risk")
            .all(|i| i.confidence == "upper-bound"));
    }

    #[test]
    fn drops_zero_dollar_diagnostics() {
        let plan = action_plan(&mixed_run(), &flat_pricing());
        // The below-minimum cache_control step is a $0 diagnostic — never an action item.
        assert!(!plan.items.iter().any(|i| i.kind == "cache-scorecard"));
    }

    #[test]
    fn keeps_capped_potential_and_at_risk_total_separate() {
        let plan = action_plan(&mixed_run(), &flat_pricing());
        // The legacy-named potential field is bounded by spend and does not absorb at-risk exposure.
        assert!(plan.recoverable_floor_micros <= plan.total_spend_micros);
        assert!(
            plan.recoverable_floor_micros > 0,
            "the wasted write is recoverable"
        );
        assert!(plan.at_risk_total_micros >= 15_000, "the batch exposure");
        // The two are distinct fields, never summed: potential alone stays ≤ spend even though
        // at_risk pushes the naive total above it.
        let naive = plan.recoverable_floor_micros + plan.at_risk_total_micros;
        assert!(naive > plan.recoverable_floor_micros);
    }

    #[test]
    fn is_deterministic() {
        let a = action_plan(&mixed_run(), &flat_pricing());
        let b = action_plan(&mixed_run(), &flat_pricing());
        assert_eq!(a, b);
    }

    #[test]
    fn empty_runs_yield_an_empty_plan() {
        let plan = action_plan(&[], &flat_pricing());
        assert!(plan.items.is_empty());
        assert_eq!(plan.recoverable_floor_micros, 0);
        assert_eq!(plan.at_risk_total_micros, 0);
        assert_eq!(plan.savings_index, 100);
        assert!(plan.estimated);
    }
}
