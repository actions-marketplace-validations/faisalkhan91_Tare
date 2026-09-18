//! Reasoning/thinking-token breakout. Reasoning ("thinking") tokens are billed as
//! output tokens but are invisible in the response body — providers charge for the full thinking
//! even when it's summarized or omitted. Tare already captures `usage.reasoning` as a distinct
//! subset of `output`; this splits output spend into ANSWER vs REASONING so a user can see
//! "reasoning was N% of my output spend" — something the provider dashboards can't show.
//!
//! Pure, integer, estimate-only. Reasoning is priced at the output rate (that's how it's billed);
//! answer = output − reasoning. Unpriced steps are skipped (a gap, never a fabricated $0).

use crate::model::{CacheClass, RunRecord};
use crate::money::MicroUsd;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One model's answer/reasoning output split.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningModel {
    pub model: String,
    pub reasoning_tokens: u64,
    pub reasoning_micros: i64,
    pub output_micros: i64,
}

/// Output-spend split across a run set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningBreakout {
    pub reasoning_tokens: u64,
    pub answer_tokens: u64,
    /// Reasoning priced at the output rate.
    pub reasoning_micros: i64,
    /// Total output spend (answer + reasoning), the denominator for the percentage.
    pub output_micros: i64,
    /// Reasoning as an integer percent of output spend (0..100), 0 when there's no output spend.
    pub reasoning_pct: i64,
    /// Per-model, largest reasoning spend first (ties broken by model name for determinism).
    pub by_model: Vec<ReasoningModel>,
}

/// Split output spend into answer vs reasoning across the run set. Skips unpriced models.
pub fn reasoning_breakout(runs: &[RunRecord], pricing: &PricingTable) -> ReasoningBreakout {
    let mut per_model: BTreeMap<String, (u64, i64, i64)> = BTreeMap::new(); // model -> (r_tok, r_mic, out_mic)
    let mut out = ReasoningBreakout::default();
    for run in runs {
        for step in &run.steps {
            if step.usage.output == 0 {
                continue;
            }
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            let input_total = step.usage.total_prompt();
            let output_rate = rates
                .for_input(input_total)
                .micro_per_mtok(CacheClass::Output);
            let reasoning = step.usage.reasoning.min(step.usage.output);
            let answer = step.usage.output - reasoning;
            let reasoning_micros = MicroUsd::for_tokens(reasoning, output_rate).micros();
            let output_micros = MicroUsd::for_tokens(step.usage.output, output_rate).micros();
            out.reasoning_tokens = out.reasoning_tokens.saturating_add(reasoning);
            out.answer_tokens = out.answer_tokens.saturating_add(answer);
            out.reasoning_micros = out.reasoning_micros.saturating_add(reasoning_micros);
            out.output_micros = out.output_micros.saturating_add(output_micros);
            let e = per_model.entry(step.model.clone()).or_insert((0, 0, 0));
            e.0 = e.0.saturating_add(reasoning);
            e.1 = e.1.saturating_add(reasoning_micros);
            e.2 = e.2.saturating_add(output_micros);
        }
    }
    out.reasoning_pct = if out.output_micros > 0 {
        ((out.reasoning_micros as i128) * 100 / out.output_micros as i128) as i64
    } else {
        0
    };
    out.by_model = per_model
        .into_iter()
        .map(|(model, (rt, rm, om))| ReasoningModel {
            model,
            reasoning_tokens: rt,
            reasoning_micros: rm,
            output_micros: om,
        })
        .collect();
    out.by_model.sort_by(|a, b| {
        b.reasoning_micros
            .cmp(&a.reasoning_micros)
            .then(a.model.cmp(&b.model))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, RequestShape, StepRecord, UsageTokens};

    fn step(model: &str, output: u64, reasoning: u64) -> StepRecord {
        StepRecord {
            run_id: "r1".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: model.into(),
            usage: UsageTokens {
                output,
                reasoning,
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

    fn priced() -> PricingTable {
        // output $10/Mtok.
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":10000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn splits_reasoning_from_answer_output() {
        // 1,000,000 output of which 400,000 reasoning, at $10/Mtok output.
        let mut run = RunRecord::new("r1");
        run.steps.push(step("m", 1_000_000, 400_000));
        let b = reasoning_breakout(std::slice::from_ref(&run), &priced());
        assert_eq!(b.reasoning_tokens, 400_000);
        assert_eq!(b.answer_tokens, 600_000);
        assert_eq!(b.output_micros, 10_000_000); // 1M × $10/Mtok
        assert_eq!(b.reasoning_micros, 4_000_000); // 0.4M × $10/Mtok
        assert_eq!(b.reasoning_pct, 40);
        assert_eq!(b.by_model[0].model, "m");
    }

    #[test]
    fn zero_reasoning_and_unpriced_are_honest() {
        let mut run = RunRecord::new("r1");
        run.steps.push(step("m", 500_000, 0)); // no reasoning
        run.steps.push(step("unpriced", 500_000, 100_000)); // skipped
        let b = reasoning_breakout(std::slice::from_ref(&run), &priced());
        assert_eq!(b.reasoning_tokens, 0);
        assert_eq!(b.reasoning_pct, 0);
        assert_eq!(b.output_micros, 5_000_000, "only the priced model's output");
    }

    #[test]
    fn hostile_counts_saturate_without_corrupting_the_percentage() {
        let mut run = RunRecord::new("r1");
        run.steps = vec![step("m", u64::MAX, u64::MAX), step("m", u64::MAX, u64::MAX)];
        let breakout = reasoning_breakout(&[run], &priced());
        assert_eq!(breakout.reasoning_tokens, u64::MAX);
        assert_eq!(breakout.answer_tokens, 0);
        assert_eq!(breakout.reasoning_micros, i64::MAX);
        assert_eq!(breakout.output_micros, i64::MAX);
        assert_eq!(breakout.reasoning_pct, 100);
        assert_eq!(breakout.by_model[0].reasoning_tokens, u64::MAX);
    }
}
