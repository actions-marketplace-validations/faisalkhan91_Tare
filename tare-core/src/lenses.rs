//! Headline cost lenses for the Overview scorecard — pure, integer micro-USD, deterministic.
//! Three FinOps framings computed by re-aggregating stored usage against pricing (counts-only):
//!   - talk-vs-listen: input (fresh + cache read/write) versus output spend,
//!   - cache savings realized: what cache reads saved versus paying the fresh rate,
//!   - cost-per-call: mean micro-USD per captured step (#14 unit economics).
//!
//! Unpriced steps contribute tokens but $0 (same honest treatment as the rest of the app).

use crate::account::{allocate_step, cost_from_usage};
use crate::model::{CacheClass, Component, RunRecord};
use crate::money::scaled_div;
use crate::money::MicroUsd;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lenses {
    /// Input-side spend (fresh + cache write + cache read).
    pub input_micros: i64,
    /// Output-side spend (output, incl. reasoning which bills at the output rate).
    pub output_micros: i64,
    /// Realized cache savings: cache-read tokens priced at (fresh − cache-read), i.e. what reading
    /// the cached prefix saved vs paying full freight. Never negative.
    pub cache_saved_micros: i64,
    pub total_micros: i64,
    /// Captured steps (calls).
    pub calls: u64,
    /// Mean micro-USD per call (integer; 0 when no calls).
    pub micros_per_call: i64,
    /// Token counts (ALL steps, priced or not — tokens exist regardless of pricing). Power the
    /// efficiency lenses: blended $/1M tokens, output-token share, and cache-miss rate.
    #[serde(default)]
    pub total_tokens: u64,
    /// Output tokens (incl. reasoning, which bills at the output rate).
    #[serde(default)]
    pub output_tokens: u64,
    /// Input-side tokens (fresh + cache write + cache read).
    #[serde(default)]
    pub input_tokens: u64,
    /// Tokens served from cache reads (the realized-cache numerator).
    #[serde(default)]
    pub cache_read_tokens: u64,
    pub pricing_version: String,
    pub estimated: bool,
}

pub fn lenses(runs: &[RunRecord], pricing: &PricingTable) -> Lenses {
    let mut input = 0i64;
    let mut output = 0i64;
    let mut saved = 0i64;
    let mut total = 0i64;
    let mut calls = 0u64;
    let mut total_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut input_tokens = 0u64;
    let mut cache_read_tokens = 0u64;
    for run in runs {
        for step in &run.steps {
            calls = calls.saturating_add(1);
            // Tokens accumulate for every step, priced or not — they exist regardless of pricing.
            // Include audio sub-classes: their tokens are stripped out of fresh_input/output at
            // parse time, but their COST is billed into total_micros — so the blended $/1M-token
            // denominator must count them too, or the unit price is overstated for multimodal runs.
            let u = &step.usage;
            input_tokens = input_tokens
                .saturating_add(u.total_prompt())
                .saturating_add(u.audio_input);
            output_tokens = output_tokens
                .saturating_add(u.output)
                .saturating_add(u.audio_output);
            total_tokens = total_tokens
                .saturating_add(u.total())
                .saturating_add(u.audio_input)
                .saturating_add(u.audio_output);
            cache_read_tokens = cache_read_tokens.saturating_add(u.cache_read);
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue; // unpriced: tokens but $0
            };
            let c = cost_from_usage(&step.usage, rates);
            input = input.saturating_add((c.fresh + c.cache_write + c.cache_read).micros());
            output = output.saturating_add(c.output.micros());
            total = total.saturating_add(c.total.micros());
            // Realized cache savings: reading N cached tokens at the read rate instead of fresh.
            if step.usage.cache_read > 0 {
                let fresh_rate = rates.micro_per_mtok(CacheClass::Fresh);
                let read_rate = rates.micro_per_mtok(CacheClass::CacheRead);
                let delta = fresh_rate.saturating_sub(read_rate).max(0);
                saved = saved
                    .saturating_add(MicroUsd::for_tokens(step.usage.cache_read, delta).micros());
            }
        }
    }
    Lenses {
        input_micros: input,
        output_micros: output,
        cache_saved_micros: saved,
        total_micros: total,
        calls,
        micros_per_call: scaled_div(total, 1, calls).unwrap_or(0),
        total_tokens,
        output_tokens,
        input_tokens,
        cache_read_tokens,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

// ---- sandwich: one prompt-component's cost across all runs ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandwichRun {
    pub run_id: String,
    pub micros: i64,
    pub tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandwich {
    pub component: String,
    pub total_micros: i64,
    /// Per-run cost of the chosen component, largest-first (only runs where it appears).
    pub runs: Vec<SandwichRun>,
    pub pricing_version: String,
    pub estimated: bool,
}

/// "This component everywhere": the cost of one prompt component (system / tools / history / …)
/// across all runs, so you can chase a single cost source instead of opening runs one by one.
/// Reuses the per-step `allocate_step` attribution; sums the chosen component per run.
pub fn component_sandwich(runs: &[RunRecord], pricing: &PricingTable, component: &str) -> Sandwich {
    let target = Component::parse(component);
    let mut out = Vec::new();
    let mut total = 0i64;
    for run in runs {
        let mut micros = 0i64;
        let mut tokens = 0u64;
        for step in &run.steps {
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            for a in allocate_step(&step.usage, rates, &step.shape) {
                if Some(a.component) == target {
                    micros = micros.saturating_add(a.micros.micros());
                    tokens = tokens.saturating_add(a.tokens);
                }
            }
        }
        if micros > 0 || tokens > 0 {
            out.push(SandwichRun {
                run_id: run.run_id.clone(),
                micros,
                tokens,
            });
            total = total.saturating_add(micros);
        }
    }
    out.sort_by(|a, b| b.micros.cmp(&a.micros).then(a.run_id.cmp(&b.run_id)));
    Sandwich {
        component: component.to_string(),
        total_micros: total,
        runs: out,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, StepMeta, StepRecord};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    fn step(cache_read: u64) -> StepRecord {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let mut s = crate::ingest_step_ct(
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
        s.usage.cache_read = cache_read;
        s
    }

    #[test]
    fn sandwich_sums_one_component_across_runs() {
        let runs = vec![
            RunRecord {
                run_id: "a".into(),
                steps: vec![step(0)],
            },
            RunRecord {
                run_id: "b".into(),
                steps: vec![step(0), step(0)],
            },
        ];
        // The OpenAI fixture's fresh input attributes to a component; "output" always has cost.
        let sw = component_sandwich(&runs, &pricing(), "output");
        assert_eq!(sw.runs.len(), 2);
        assert!(sw.total_micros > 0);
        // Run b (2 steps) costs more on output than run a (1 step) -> ranked first.
        assert_eq!(sw.runs[0].run_id, "b");
        // Unknown component -> no matches, zero total.
        assert_eq!(
            component_sandwich(&runs, &pricing(), "nope").total_micros,
            0
        );
    }

    #[test]
    fn splits_io_and_credits_realized_cache_savings() {
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(1000), step(0)],
        }];
        let l = lenses(&runs, &pricing());
        assert_eq!(l.calls, 2);
        assert!(l.input_micros > 0 && l.output_micros > 0);
        assert_eq!(l.total_micros, l.input_micros + l.output_micros);
        // The step that read 1000 cached tokens shows positive realized savings.
        assert!(l.cache_saved_micros > 0);
        assert_eq!(l.micros_per_call, l.total_micros / 2);
    }

    #[test]
    fn accumulates_token_counts_for_efficiency_lenses() {
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(1000), step(0)],
        }];
        let l = lenses(&runs, &pricing());
        // total = input-side + output, consistent with UsageTokens::total().
        assert_eq!(l.total_tokens, l.input_tokens + l.output_tokens);
        assert!(l.total_tokens > 0 && l.output_tokens > 0 && l.input_tokens > 0);
        // One step read 1000 cached tokens -> cache_read_tokens reflects it, ≤ input.
        assert_eq!(l.cache_read_tokens, 1000);
        assert!(l.cache_read_tokens <= l.input_tokens);
    }

    #[test]
    fn token_counts_include_audio_so_blended_price_isnt_overstated() {
        // Audio tokens are billed into total_micros but stripped from fresh_input/output at parse
        // time; the efficiency denominators must still count them (review fix).
        let mut s = step(0);
        let base_total = s.usage.total();
        s.usage.audio_input = 700;
        s.usage.audio_output = 300;
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![s],
        }];
        let l = lenses(&runs, &pricing());
        assert_eq!(l.total_tokens, base_total + 1000, "audio counted in total");
        assert_eq!(
            l.total_tokens,
            l.input_tokens + l.output_tokens,
            "still consistent"
        );
        assert!(
            l.input_tokens >= 700 && l.output_tokens >= 300,
            "audio split into each side"
        );
    }

    #[test]
    fn token_counts_include_unpriced_steps() {
        // Tokens exist regardless of pricing; an unpriced model still contributes token counts.
        let mut s = step(0);
        s.model = "totally-unpriced-model".into();
        let tokens = s.usage.total();
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![s],
        }];
        let l = lenses(&runs, &pricing());
        assert_eq!(l.total_micros, 0, "unpriced -> $0");
        assert_eq!(l.total_tokens, tokens, "but tokens still counted");
    }
}
