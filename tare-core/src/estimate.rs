//! Pre-flight cost band from a stored shape: reprice a run you already captured to
//! answer "what will a run like this cost?" BEFORE you spend it — no model call, no re-execution,
//! no payload. Seeding from a CAPTURED shape (real token counts + cache classes) sidesteps the two
//! ways a hand-guessed estimate lies:
//!   1. **Tool-use overhead** — the tool/system scaffolding adds ~290–804 tokens per request that a
//!      bare prompt count omits (verified against Anthropic tool-use token accounting).
//!   2. **Cross-tokenizer drift** — repricing onto a *different* model holds the captured counts,
//!      but that model tokenizes differently (Opus 4.7+ run ~+30% tokens vs older families), so the
//!      real bill would land higher.
//!
//! So the estimate is a BAND, honestly asymmetric: the captured cost is a floor (counts undercount),
//! and the high bound adds the tool-use overhead and — when estimating a model swap — the
//! cross-tokenizer inflation. Pure, integer, clock-free.

use crate::attribute::build_report;
use crate::model::{Provider, RunRecord, UsageTokens};
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};

/// Verified upper bound on tool-use system-prompt overhead, in tokens per request. Added to the
/// high bound because bare captured input counts omit the tool/system scaffolding.
pub const TOOL_USE_OVERHEAD_HIGH_TOK: u64 = 804;

/// Cross-tokenizer inflation applied to the high bound when repricing onto a newer-generation model
/// (percent). Opus 4.7+ tokenizers run ~30% more tokens than the baseline on the same text. Sourced
/// from [`crate::tokenizer::INFLATED_GENERATION_PCT`] so the estimator and the per-model note agree.
pub const CROSS_TOKENIZER_INFLATION_PCT: u64 = crate::tokenizer::INFLATED_GENERATION_PCT;

/// A pre-flight cost band for a run shaped like a captured one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    pub run_id: String,
    /// The model the estimate is priced against (the swap target, or the captured model).
    pub model: String,
    /// Repriced captured counts as-is — the FLOOR (captured counts undercount the real request).
    pub low_micros: i64,
    /// Point estimate: same as `low_micros` (the captured shape at face value).
    pub point_micros: i64,
    /// High bound: adds tool-use overhead per step, plus cross-tokenizer inflation on a model swap.
    pub high_micros: i64,
    /// True if this is a cross-model estimate (band widened by the tokenizer caveat).
    pub cross_tokenizer: bool,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Estimate the cost band of a run shaped like `run`. `target` reprices onto a different model
/// (its rates must be bundled); `None` keeps the captured model. Errors if the target is unpriced
/// (never a fabricated $0).
pub fn estimate_like(
    run: &RunRecord,
    pricing: &PricingTable,
    target: Option<&str>,
) -> Result<Estimate, String> {
    // Resolve the priced model + whether this is a cross-model (tokenizer-caveat) estimate.
    let (priced_run, model, cross) = match target {
        None => (clone_run(run, None), captured_model(run), false),
        Some(t) => {
            let rates = pricing.find_by_model(t).ok_or_else(|| {
                format!("estimate target model {t:?} has no bundled price; refusing to render $0")
            })?;
            let provider = Provider::parse(&rates.provider);
            // Vendor-only (OpenAI-compatible) rows carry a free-text provider tag (e.g. "groq"),
            // not a Provider enum. clone_run has no vendor plumbing, so it would leave each step on
            // its ORIGINAL provider with a model that provider can't price → unpriced → a $0 phantom
            // Refuse it, mirroring whatif::resolve_target.
            if provider.is_none() {
                return Err(format!(
                    "estimate target {t:?} is only priced under OpenAI-compatible vendor {:?}; \
                     vendor swap targets aren't supported yet (repricing would silently render $0)",
                    rates.provider
                ));
            }
            let cross = t != captured_model(run);
            (
                clone_run(run, Some((provider, rates.model_id.clone()))),
                rates.model_id.clone(),
                cross,
            )
        }
    };

    let point = build_report(std::slice::from_ref(&priced_run), pricing).total_micros;

    // High bound: inflate the shape. Tool-use overhead is added to each step's fresh input; the
    // cross-tokenizer factor scales every captured token axis. The factor applies only when the swap
    // crosses INTO a newer (Opus 4.7+) tokenizer generation — the genuine ~30%
    // under-count direction — and stays conservative (inflate) when either generation is unknown, so
    // we never narrow the band out of ignorance. A same-or-lower-generation swap doesn't over-inflate.
    let inflate = cross && crate::tokenizer::crosses_into_inflated(&captured_model(run), &model);
    let factor_num = if inflate {
        100 + CROSS_TOKENIZER_INFLATION_PCT
    } else {
        100
    };
    let mut high_run = priced_run.clone();
    for step in &mut high_run.steps {
        let u = &mut step.usage;
        let scale = |t: u64| t.saturating_mul(factor_num) / 100;
        *u = UsageTokens {
            fresh_input: scale(u.fresh_input).saturating_add(TOOL_USE_OVERHEAD_HIGH_TOK),
            cache_write_5m: scale(u.cache_write_5m),
            cache_write_1h: scale(u.cache_write_1h),
            cache_read: scale(u.cache_read),
            output: scale(u.output),
            reasoning: scale(u.reasoning),
            audio_input: scale(u.audio_input),
            audio_output: scale(u.audio_output),
        };
    }
    let high = build_report(std::slice::from_ref(&high_run), pricing).total_micros;

    Ok(Estimate {
        run_id: run.run_id.clone(),
        model,
        low_micros: point,
        point_micros: point,
        high_micros: high.max(point),
        cross_tokenizer: cross,
        pricing_version: pricing.version.clone(),
        estimated: true,
    })
}

fn captured_model(run: &RunRecord) -> String {
    run.steps
        .first()
        .map(|s| s.model.clone())
        .unwrap_or_default()
}

/// Clone a run, optionally rewriting every step onto a target (provider, model).
fn clone_run(run: &RunRecord, target: Option<(Option<Provider>, String)>) -> RunRecord {
    let mut steps = run.steps.clone();
    if let Some((provider, model)) = target {
        for s in &mut steps {
            if let Some(p) = provider {
                s.provider = p;
            }
            s.model = model.clone();
        }
    }
    RunRecord {
        run_id: run.run_id.clone(),
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheTtl, RequestShape, StepRecord};

    fn pricing() -> PricingTable {
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"big","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000},
              {"provider":"anthropic","model_id":"small","input_micro_per_mtok":1000000,
               "output_micro_per_mtok":5000000,"cache_read_micro_per_mtok":100000,
               "cache_write_5m_micro_per_mtok":1250000,"cache_write_1h_micro_per_mtok":2000000}]}"#,
        )
        .unwrap()
    }

    fn run(model: &str, input: u64, output: u64) -> RunRecord {
        let step = StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: model.into(),
            usage: UsageTokens {
                fresh_input: input,
                output,
                ..Default::default()
            },
            shape: RequestShape {
                model: model.into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
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
            run_id: "r".into(),
            steps: vec![step],
        }
    }

    #[test]
    fn same_model_band_is_floor_plus_tool_overhead() {
        // 1M input × $3 + 1M output × $15 = $18 point.
        let e = estimate_like(&run("big", 1_000_000, 1_000_000), &pricing(), None).unwrap();
        assert_eq!(e.model, "big");
        assert!(!e.cross_tokenizer);
        assert_eq!(e.point_micros, 18_000_000);
        assert_eq!(e.low_micros, 18_000_000);
        // High adds 804 input tokens × $3/Mtok = 2412 micros over the floor.
        assert_eq!(e.high_micros, 18_000_000 + 804 * 3);
        assert!(e.high_micros >= e.low_micros);
    }

    #[test]
    fn cross_model_widens_the_band_by_the_tokenizer_caveat() {
        // Reprice "big" run onto cheaper "small": point uses captured counts at small's rates.
        let e =
            estimate_like(&run("big", 1_000_000, 1_000_000), &pricing(), Some("small")).unwrap();
        assert_eq!(e.model, "small");
        assert!(e.cross_tokenizer);
        // point: 1M×$1 + 1M×$5 = $6.
        assert_eq!(e.point_micros, 6_000_000);
        // high: tokens ×1.3 + 804 overhead on input, at small's rates.
        // input (1.3M + 804) × $1/Mtok = 1_300_804 micros; output 1.3M × $5/Mtok = 6_500_000.
        assert_eq!(e.high_micros, 1_300_804 + 6_500_000);
        assert!(
            e.high_micros > e.point_micros,
            "cross-model band is wider than the point"
        );
    }

    #[test]
    fn unpriced_target_errors_rather_than_zeroing() {
        let err = estimate_like(&run("big", 100, 100), &pricing(), Some("ghost"));
        assert!(err.is_err());
    }

    #[test]
    fn vendor_only_target_errors_rather_than_fabricating_zero() {
        // `llama-3.1-70b` is priced ONLY under OpenAI-compatible vendor rows (provider "groq"/
        // "together"), which the reprice path can't target — must Err, not render a $0.
        let err = estimate_like(&run("big", 100, 100), &pricing(), Some("llama-3.1-70b"));
        assert!(
            err.is_err(),
            "vendor-only target must error, not fabricate $0"
        );
    }
}
