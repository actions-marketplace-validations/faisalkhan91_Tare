//! Realized cache-savings ledger. Where `savings.rs` *projects* what prompt-caching
//! could save, this reports what caching ALREADY saved: every `cache_read` token was billed at the
//! cache-read rate (0.1× base input on Anthropic) instead of the 1× base-input rate it would have
//! cost as fresh input. That difference, summed, is money the user already kept — the inverse of the
//! trim-list, and the first proof that Tare's advice pays off.
//!
//! The counterfactual is the **base input rate**, NOT the cache-write rate: a cache write is a
//! separate one-time sunk cost, so charging the reads' baseline against the write would misframe the
//! figure. Pure, integer, estimate-only; unpriced steps are skipped (a gap, never a fabricated $0).

use crate::model::{CacheClass, RunRecord};
use crate::money::MicroUsd;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One model's realized cache savings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheModelSaving {
    pub model: String,
    pub cache_read_tokens: u64,
    pub saved_micros: i64,
}

/// Realized savings from cache reads across a run set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheLedger {
    pub cache_read_tokens: u64,
    /// Money already kept: `cache_read × (base_input_rate − cache_read_rate)`, summed. Never negative.
    pub saved_micros: i64,
    /// What those reads actually cost (at the cache-read rate) — the denominator for a "saved N×"
    /// framing, and an honesty anchor (the ledger is a delta over this real spend).
    pub read_cost_micros: i64,
    /// Per-model breakdown, largest saving first (ties broken by model name for determinism).
    pub by_model: Vec<CacheModelSaving>,
}

/// Compute the realized cache-savings ledger. Skips steps with no cache read and unpriced models.
/// Uses the same tier-resolved rates as the cost engine (`for_input`) so the baseline matches how the
/// run was actually priced.
pub fn cache_ledger(runs: &[RunRecord], pricing: &PricingTable) -> CacheLedger {
    let mut per_model: BTreeMap<String, (u64, i64)> = BTreeMap::new();
    let mut out = CacheLedger::default();
    for run in runs {
        for step in &run.steps {
            if step.usage.cache_read == 0 {
                continue;
            }
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue; // unpriced → a gap, not a fake zero
            };
            let input_total = step.usage.total_prompt();
            let resolved = rates.for_input(input_total);
            let counterfactual = MicroUsd::for_tokens(
                step.usage.cache_read,
                resolved.micro_per_mtok(CacheClass::Fresh),
            )
            .micros();
            let actual = MicroUsd::for_tokens(
                step.usage.cache_read,
                resolved.micro_per_mtok(CacheClass::CacheRead),
            )
            .micros();
            let saved = counterfactual.saturating_sub(actual).max(0);
            out.cache_read_tokens = out.cache_read_tokens.saturating_add(step.usage.cache_read);
            out.saved_micros = out.saved_micros.saturating_add(saved);
            out.read_cost_micros = out.read_cost_micros.saturating_add(actual);
            let e = per_model.entry(step.model.clone()).or_insert((0, 0));
            e.0 = e.0.saturating_add(step.usage.cache_read);
            e.1 = e.1.saturating_add(saved);
        }
    }
    out.by_model = per_model
        .into_iter()
        .map(|(model, (t, s))| CacheModelSaving {
            model,
            cache_read_tokens: t,
            saved_micros: s,
        })
        .collect();
    out.by_model.sort_by(|a, b| {
        b.saved_micros
            .cmp(&a.saved_micros)
            .then(a.model.cmp(&b.model))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, RequestShape, StepRecord, UsageTokens};

    fn step(model: &str, cache_read: u64) -> StepRecord {
        StepRecord {
            run_id: "r1".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: model.into(),
            usage: UsageTokens {
                cache_read,
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
        }
    }

    #[test]
    fn empty_when_no_cache_reads() {
        let pricing = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[]}"#,
        )
        .unwrap();
        assert_eq!(cache_ledger(&[], &pricing), CacheLedger::default());
    }

    #[test]
    fn realized_savings_is_reads_times_rate_delta() {
        // One priced model: input $3/Mtok, cache-read $0.30/Mtok. 1,000,000 cache-read tokens →
        // actual = $0.30, counterfactual (as fresh) = $3.00, saved = $2.70.
        let pricing = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000}]}"#,
        )
        .unwrap();
        let mut run = RunRecord::new("r1");
        run.steps.push(step("m", 1_000_000));
        let led = cache_ledger(std::slice::from_ref(&run), &pricing);
        assert_eq!(led.cache_read_tokens, 1_000_000);
        assert_eq!(led.saved_micros, 2_700_000); // (3.00 − 0.30) × 1M tok
        assert_eq!(led.read_cost_micros, 300_000);
        assert_eq!(led.by_model.len(), 1);
        assert_eq!(led.by_model[0].model, "m");
        assert_eq!(led.by_model[0].saved_micros, 2_700_000);
    }

    #[test]
    fn unpriced_model_is_skipped_not_zeroed() {
        let pricing = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[]}"#,
        )
        .unwrap();
        let mut run = RunRecord::new("r1");
        run.steps.push(step("unpriced", 500_000));
        let led = cache_ledger(std::slice::from_ref(&run), &pricing);
        assert_eq!(led.saved_micros, 0);
        assert_eq!(
            led.cache_read_tokens, 0,
            "unpriced step contributes nothing, not a fake $0"
        );
    }

    #[test]
    fn hostile_counts_saturate_in_totals_and_per_model_rows() {
        let pricing = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":0,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":0,"cache_write_1h_micro_per_mtok":0}]}"#,
        )
        .unwrap();
        let mut run = RunRecord::new("r1");
        run.steps = (0..3).map(|_| step("m", u64::MAX)).collect();

        let ledger = cache_ledger(&[run], &pricing);
        assert_eq!(ledger.cache_read_tokens, u64::MAX);
        assert_eq!(ledger.saved_micros, i64::MAX);
        assert_eq!(ledger.read_cost_micros, i64::MAX);
        assert_eq!(ledger.by_model[0].cache_read_tokens, u64::MAX);
        assert_eq!(ledger.by_model[0].saved_micros, i64::MAX);
    }
}
