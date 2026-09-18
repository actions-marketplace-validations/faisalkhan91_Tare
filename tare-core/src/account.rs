//! Cost accounting from provider-reported usage. Integer micro-USD throughout.
//!
//! Cache cost uses THREE input rates: read 0.1x, write-5m 1.25x, write-1h 2x, with
//! the write TTL taken from the request. Per-component allocation apportions each
//! class's total cost by UTF-8 byte weight (largest-remainder, deterministic) so the
//! flamegraph node dollars sum exactly to the class totals.

use crate::model::{CacheClass, Component, Provider, RequestShape, UsageTokens};
use crate::money::MicroUsd;
use crate::pricing::{ModelRates, PricingTable};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub fresh: MicroUsd,
    pub cache_write: MicroUsd,
    pub cache_read: MicroUsd,
    pub output: MicroUsd,
    pub total: MicroUsd,
}

/// Per-request price scalars: a multiplier applied to the WHOLE per-token cost after
/// rating, uniformly across every cache class — NOT a separate rate row, so cost stays a clock-free
/// recompute from stored counts + flags. Batch API is 0.5×; a data-residency/regional surcharge is
/// e.g. +10%. Held as integer basis points (`10_000` = 1.0×) so scaling is exact and rounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceScalars {
    /// Combined multiplier in basis points (10_000 = identity).
    pub bps: u32,
}

impl Default for PriceScalars {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl PriceScalars {
    /// No adjustment (1.0×).
    pub const IDENTITY: PriceScalars = PriceScalars { bps: 10_000 };

    /// The Batch API's 50%-off multiplier.
    pub fn batch() -> Self {
        Self { bps: 5_000 }
    }

    /// Apply a residency/regional surcharge of `pct` percent (e.g. `10` → ×1.1) on top of self.
    pub fn with_residency_pct(self, pct: u32) -> Self {
        let bps = u128::from(self.bps) * (100 + u128::from(pct)) / 100;
        Self {
            bps: bps.min(u128::from(u32::MAX)) as u32,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.bps == 10_000
    }
}

impl CostBreakdown {
    /// Scale every class (and the total) by `scalars`, rounding each class to the nearest micro-USD
    /// so the parts still sum to the total. Identity scalars return an unchanged breakdown.
    pub fn scaled(self, scalars: PriceScalars) -> CostBreakdown {
        if scalars.is_identity() {
            return self;
        }
        let bps = scalars.bps as i128;
        let s = |m: MicroUsd| {
            let v = m.micros() as i128;
            // round-half-up (costs are non-negative); clamp into i64 so a saturated class + residency
            // surcharge can't wrap negative.
            MicroUsd(((v * bps + 5_000) / 10_000).clamp(i64::MIN as i128, i64::MAX as i128) as i64)
        };
        let fresh = s(self.fresh);
        let cache_write = s(self.cache_write);
        let cache_read = s(self.cache_read);
        let output = s(self.output);
        CostBreakdown {
            fresh,
            cache_write,
            cache_read,
            output,
            total: fresh + cache_write + cache_read + output,
        }
    }
}

/// Aggregate cost of one step's usage (used by the report). The cache-write TTL split
/// comes from the wire (`UsageTokens.cache_write_5m`/`_1h`), so the request shape is no
/// longer consulted for billing.
pub fn cost_usage(usage: &UsageTokens, rates: &ModelRates, _shape: &RequestShape) -> CostBreakdown {
    cost_from_usage(usage, rates)
}

/// As [`cost_from_usage`], then apply per-request [`PriceScalars`] (batch/residency) uniformly
/// across all classes. Kept separate so the default costing path is byte-identical.
pub fn cost_from_usage_scaled(
    usage: &UsageTokens,
    rates: &ModelRates,
    scalars: PriceScalars,
) -> CostBreakdown {
    cost_from_usage(usage, rates).scaled(scalars)
}

/// Shape-free cost of a usage vector against a rates row. This is the kernel `cost_usage`
/// delegates to, and the basis for the embeddable `account_from_usage` facade — billing
/// never needs the request shape.
pub fn cost_from_usage(usage: &UsageTokens, rates: &ModelRates) -> CostBreakdown {
    // Apply context-window pricing: total input decides whether the long-context tier kicks in.
    let input_total = usage.total_prompt(); // saturating: a corrupt/huge count can't wrap the tier check
    let resolved = rates.for_input(input_total);
    let rates = &resolved;
    let fresh = MicroUsd::for_tokens(usage.fresh_input, rates.micro_per_mtok(CacheClass::Fresh));
    let cache_write = MicroUsd::for_tokens(
        usage.cache_write_5m,
        rates.micro_per_mtok(CacheClass::CacheWrite5m),
    ) + MicroUsd::for_tokens(
        usage.cache_write_1h,
        rates.micro_per_mtok(CacheClass::CacheWrite1h),
    );
    let cache_read = MicroUsd::for_tokens(
        usage.cache_read,
        rates.micro_per_mtok(CacheClass::CacheRead),
    );
    let mut output = MicroUsd::for_tokens(usage.output, rates.micro_per_mtok(CacheClass::Output));
    // Multimodal audio sub-classes (priced at their own rate; 0 for text-only models). Folded into
    // the input/output halves so the two-way split and totals stay coherent.
    let fresh = fresh + MicroUsd::for_tokens(usage.audio_input, rates.audio_input_micro_per_mtok);
    output += MicroUsd::for_tokens(usage.audio_output, rates.audio_output_micro_per_mtok);
    CostBreakdown {
        fresh,
        cache_write,
        cache_read,
        output,
        total: fresh + cache_write + cache_read + output,
    }
}

/// **Embeddable accounting facade.** Cost a provider-reported usage vector against a
/// pricing table, with NO raw bytes and NO re-tokenization — exactly the costing a caller who
/// already parsed the provider's `usage` block would get from the full pipeline. Returns `None`
/// when the model is unpriced (a gap, never a fabricated $0).
///
/// This is the semver-stable entry point for embedding Tare's integer-money kernel in another
/// tool without the proxy or CLI. Its signature is locked by the facade-API golden test.
pub fn account_from_usage(
    provider: Provider,
    model: &str,
    usage: &UsageTokens,
    pricing: &PricingTable,
) -> Option<CostBreakdown> {
    // Facade keeps its golden-locked signature (no vendor param); OpenAI-compatible vendor pricing
    // is reachable via the step path, not this embeddable kernel entry.
    let rates = pricing.lookup(provider, None, model)?;
    Some(cost_from_usage(usage, rates))
}

/// A leaf allocation of tokens+dollars to a (component, cache class) pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alloc {
    pub component: Component,
    pub class: CacheClass,
    pub tokens: u64,
    pub micros: MicroUsd,
}

/// Largest-remainder apportionment of an integer `total` across `weights`.
/// Returns one value per weight, in input order, summing exactly to `total`.
/// Shared by `account` and `attribute` as their single source of truth.
pub fn apportion(total: u64, weights: &[u64]) -> Vec<u64> {
    let sum: u128 = weights.iter().map(|&w| w as u128).sum();
    if sum == 0 || weights.is_empty() {
        // No weight info: put everything on the first slot (or nothing if empty).
        let mut out = vec![0u64; weights.len()];
        if let Some(first) = out.first_mut() {
            *first = total;
        }
        return out;
    }
    let t = total as u128;
    let mut base: Vec<u64> = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(u128, usize)> = Vec::with_capacity(weights.len());
    let mut allocated: u128 = 0;
    for (i, &w) in weights.iter().enumerate() {
        let num = t * w as u128;
        let b = num / sum;
        let r = num % sum;
        base.push(b as u64);
        remainders.push((r, i));
        allocated += b;
    }
    let mut leftover = t - allocated;
    // Distribute leftover to the largest remainders; ties broken by lower index
    // (which corresponds to Component Ord order — deterministic).
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, idx) in remainders.iter() {
        if leftover == 0 {
            break;
        }
        base[idx] += 1;
        leftover -= 1;
    }
    base
}

/// Apportion a micro-USD total across slots by token weight (largest remainder).
fn apportion_micros(total: MicroUsd, token_weights: &[u64]) -> Vec<MicroUsd> {
    if total.0 < 0 {
        // Not expected in v0.1; keep deterministic anyway.
        let parts = apportion(total.0.unsigned_abs(), token_weights);
        return parts
            .into_iter()
            .map(|p| {
                if p > i64::MAX as u64 {
                    MicroUsd(i64::MIN)
                } else {
                    MicroUsd(-(p as i64))
                }
            })
            .collect();
    }
    apportion(total.0 as u64, token_weights)
        .into_iter()
        .map(|p| MicroUsd(p as i64))
        .collect()
}

/// Per-(component, class) allocation for the flamegraph. Deterministically ordered.
pub fn allocate_step(usage: &UsageTokens, rates: &ModelRates, shape: &RequestShape) -> Vec<Alloc> {
    // Resolve the context-window tier so per-component flamegraph dollars match the report total.
    let input_total = usage.total_prompt(); // saturating: a corrupt/huge count can't wrap the tier check
    let resolved = rates.for_input(input_total);
    let rates = &resolved;
    let mut out: Vec<Alloc> = Vec::new();

    // Fresh input apportioned across structural components by byte weight.
    if usage.fresh_input > 0 {
        let comps: Vec<Component> = shape.weights.iter().map(|w| w.component).collect();
        let weights: Vec<u64> = shape.weights.iter().map(|w| w.bytes).collect();
        let (comps, weights) = if comps.is_empty() {
            // No structurally-attributable components -> dedicated Other bucket (never System).
            (vec![Component::Other], vec![1u64])
        } else {
            (comps, weights)
        };
        let tok = apportion(usage.fresh_input, &weights);
        let fresh_total =
            MicroUsd::for_tokens(usage.fresh_input, rates.micro_per_mtok(CacheClass::Fresh));
        let mic = apportion_micros(fresh_total, &tok);
        for i in 0..comps.len() {
            if tok[i] == 0 {
                continue;
            }
            out.push(Alloc {
                component: comps[i],
                class: CacheClass::Fresh,
                tokens: tok[i],
                micros: mic[i],
            });
        }
    }

    // Cache write/read attributed to the cached prefix component. When the request body wasn't seen
    // (out-of-band JSONL/OTel → cached_component=None), the whole cached prefix is an honest aggregate
    // (system + tools + history), NOT the system prompt. Writes are split by TTL tier.
    let cached_comp = shape.cached_component.unwrap_or(Component::CachedPrefix);
    for (tokens, class) in [
        (usage.cache_write_5m, CacheClass::CacheWrite5m),
        (usage.cache_write_1h, CacheClass::CacheWrite1h),
    ] {
        if tokens > 0 {
            out.push(Alloc {
                component: cached_comp,
                class,
                tokens,
                micros: MicroUsd::for_tokens(tokens, rates.micro_per_mtok(class)),
            });
        }
    }
    if usage.cache_read > 0 {
        out.push(Alloc {
            component: cached_comp,
            class: CacheClass::CacheRead,
            tokens: usage.cache_read,
            micros: MicroUsd::for_tokens(
                usage.cache_read,
                rates.micro_per_mtok(CacheClass::CacheRead),
            ),
        });
    }

    // Output, splitting reasoning out as its own class.
    if usage.output > 0 {
        let out_total =
            MicroUsd::for_tokens(usage.output, rates.micro_per_mtok(CacheClass::Output));
        let reasoning = usage.reasoning.min(usage.output);
        let plain = usage.output - reasoning;
        let mic = apportion_micros(out_total, &[plain, reasoning]);
        if plain > 0 {
            out.push(Alloc {
                component: Component::Output,
                class: CacheClass::Output,
                tokens: plain,
                micros: mic[0],
            });
        }
        if reasoning > 0 {
            out.push(Alloc {
                component: Component::Output,
                class: CacheClass::Reasoning,
                tokens: reasoning,
                micros: mic[1],
            });
        }
    }

    // Multimodal audio sub-classes (wire splits them OUT of fresh_input/output, so they live only
    // in audio_*). `cost_from_usage` folds their cost into the input/output halves; allocate_step
    // must emit matching leaves or the flamegraph allocations wouldn't sum to the report total
    // $0 today for text-only rates, non-zero once an audio-priced table is used.
    if usage.audio_input > 0 {
        out.push(Alloc {
            component: Component::Other,
            class: CacheClass::Fresh,
            tokens: usage.audio_input,
            micros: MicroUsd::for_tokens(usage.audio_input, rates.audio_input_micro_per_mtok),
        });
    }
    if usage.audio_output > 0 {
        out.push(Alloc {
            component: Component::Output,
            class: CacheClass::Output,
            tokens: usage.audio_output,
            micros: MicroUsd::for_tokens(usage.audio_output, rates.audio_output_micro_per_mtok),
        });
    }

    // Deterministic order: by component, then class.
    out.sort_by(|a, b| a.component.cmp(&b.component).then(a.class.cmp(&b.class)));
    out
}

/// Preserve a step's token/component structure when no price row exists. Unknown models are a
/// pricing gap, not absent usage: flamegraphs can render these allocations in token mode while all
/// dollar fields remain zero.
pub(crate) fn allocate_unpriced_step(usage: &UsageTokens, shape: &RequestShape) -> Vec<Alloc> {
    let zero_rates = ModelRates {
        provider: String::new(),
        model_id: String::new(),
        tier: "standard".to_string(),
        input_micro_per_mtok: 0,
        output_micro_per_mtok: 0,
        cache_read_micro_per_mtok: 0,
        cache_write_5m_micro_per_mtok: 0,
        cache_write_1h_micro_per_mtok: 0,
        audio_input_micro_per_mtok: 0,
        audio_output_micro_per_mtok: 0,
        context_tiers: Vec::new(),
        effective_date: None,
    };
    allocate_step(usage, &zero_rates, shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;
    use crate::pricing::PricingTable;
    use crate::wire;

    fn opus() -> ModelRates {
        let t = PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml"))
            .unwrap();
        t.lookup(Provider::Anthropic, None, "claude-opus-4-8")
            .unwrap()
            .clone()
    }

    #[test]
    fn price_scalars_scale_all_classes_uniformly() {
        use crate::model::UsageTokens;
        let rates = opus();
        let usage = UsageTokens {
            fresh_input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            ..Default::default()
        };
        let base = cost_from_usage(&usage, &rates);
        // Identity: unchanged.
        assert_eq!(
            cost_from_usage_scaled(&usage, &rates, PriceScalars::IDENTITY),
            base
        );
        // Batch = 0.5×: every class + total halved.
        let batch = cost_from_usage_scaled(&usage, &rates, PriceScalars::batch());
        assert_eq!(batch.fresh.micros(), (base.fresh.micros() + 1) / 2); // round-half-up
        assert_eq!(
            batch.total.micros(),
            batch.fresh.micros() + batch.cache_read.micros() + batch.output.micros()
        );
        assert!(
            batch.total.micros() * 2 >= base.total.micros() - 2
                && batch.total.micros() * 2 <= base.total.micros() + 2
        );
        // Residency +10% on top of batch → 0.55×.
        let combo = PriceScalars::batch().with_residency_pct(10);
        assert_eq!(combo.bps, 5_500);
        let scaled = base.total.micros() as i128 * 5_500 / 10_000;
        let got = cost_from_usage_scaled(&usage, &rates, combo);
        // parts-summed total is within rounding of the naive scale.
        assert!((got.total.micros() as i128 - scaled).abs() <= 3);
        // Parts always sum to total.
        assert_eq!(
            got.total.micros(),
            got.fresh.micros()
                + got.cache_write.micros()
                + got.cache_read.micros()
                + got.output.micros()
        );
        assert_eq!(
            PriceScalars { bps: u32::MAX }
                .with_residency_pct(u32::MAX)
                .bps,
            u32::MAX,
            "an extreme surcharge saturates instead of wrapping"
        );
    }

    #[test]
    fn audio_tokens_priced_at_their_own_rate() {
        use crate::model::UsageTokens;
        let mut rates = opus();
        rates.audio_input_micro_per_mtok = 40_000_000; // $40/Mtok audio in
        rates.audio_output_micro_per_mtok = 80_000_000; // $80/Mtok audio out
        let usage = UsageTokens {
            audio_input: 1_000_000,
            audio_output: 1_000_000,
            ..Default::default()
        };
        let c = cost_from_usage(&usage, &rates);
        // Audio in folds into `fresh`, audio out into `output`.
        assert_eq!(c.fresh.micros(), 40_000_000);
        assert_eq!(c.output.micros(), 80_000_000);
        assert_eq!(c.total.micros(), 120_000_000);
        // Text-only usage prices audio at $0 (additive, no regression).
        let text = UsageTokens {
            fresh_input: 1000,
            ..Default::default()
        };
        let base = cost_from_usage(&text, &opus());
        assert_eq!(
            base.total.micros(),
            cost_from_usage(&text, &rates).total.micros()
        );
    }

    #[test]
    fn empty_weights_attribute_to_other_not_system() {
        use crate::model::{CacheTtl, Component, Provider, RequestShape, UsageTokens};
        let shape = RequestShape {
            model: "claude-opus-4-8".into(),
            provider: Provider::Anthropic,
            stream: false,
            ttl: CacheTtl::FiveMin,
            has_cache_control: false,
            cached_component: None,
            system_hash: None,
            weights: vec![], // no structurally-attributable components
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
        };
        let usage = UsageTokens {
            fresh_input: 100,
            ..Default::default()
        };
        let allocs = allocate_step(&usage, &opus(), &shape);
        assert!(allocs
            .iter()
            .any(|a| a.component == Component::Other && a.tokens == 100));
        assert!(!allocs.iter().any(|a| a.component == Component::System));
    }

    #[test]
    fn out_of_band_cache_is_labeled_cached_prefix_not_system() {
        use crate::model::{CacheClass, CacheTtl, Component, Provider, RequestShape, UsageTokens};
        // JSONL/OTel: cached_component=None, no weights. The cached prefix must be the honest aggregate
        // (system + tools + history), NOT mislabeled as the system prompt.
        let shape = RequestShape {
            model: "claude-opus-4-8".into(),
            provider: Provider::Anthropic,
            stream: false,
            ttl: CacheTtl::FiveMin,
            has_cache_control: true,
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
        };
        let usage = UsageTokens {
            fresh_input: 100,
            cache_read: 50_000,
            cache_write_5m: 10_000,
            output: 200,
            ..Default::default()
        };
        let allocs = allocate_step(&usage, &opus(), &shape);
        let cache: Vec<_> = allocs
            .iter()
            .filter(|a| {
                matches!(
                    a.class,
                    CacheClass::CacheRead | CacheClass::CacheWrite5m | CacheClass::CacheWrite1h
                )
            })
            .collect();
        assert!(!cache.is_empty(), "the session has cached tokens");
        assert!(
            cache.iter().all(|a| a.component == Component::CachedPrefix),
            "the cached prefix is the honest aggregate, not System"
        );
        assert!(
            allocs.iter().all(|a| a.component != Component::System),
            "nothing is mislabeled System when the body was never seen"
        );
    }

    #[test]
    fn apportion_sums_to_total() {
        let parts = apportion(100, &[1, 1, 1]);
        assert_eq!(parts.iter().sum::<u64>(), 100);
        // 34/33/33 with leftover to first by index tie-break
        assert_eq!(parts, vec![34, 33, 33]);
        assert_eq!(
            apportion_micros(MicroUsd(i64::MIN), &[1]),
            vec![MicroUsd(i64::MIN)]
        );
    }

    #[test]
    fn stream_write5m_cost_matches_hand_calc() {
        let req = include_bytes!("../../fixtures/anthropic_stream/request.json");
        let resp = include_bytes!("../../fixtures/anthropic_stream/response.sse");
        let (shape, usage, _) = wire::parse_step(Provider::Anthropic, req, resp).unwrap();
        let c = cost_usage(&usage, &opus(), &shape);
        // Real capture: fresh 13 * 5/MTok = 65; write-5m 3204 * 6.25/MTok = 20025; output 64 * 25/MTok = 1600
        assert_eq!(c.fresh, MicroUsd(65));
        assert_eq!(c.cache_write, MicroUsd(20025));
        assert_eq!(c.output, MicroUsd(1600));
        assert_eq!(c.total, MicroUsd(65 + 20025 + 1600));
    }

    #[test]
    fn alloc_micros_sum_to_cost() {
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let (shape, usage, _) = wire::parse_step(Provider::Anthropic, req, resp).unwrap();
        let allocs = allocate_step(&usage, &opus(), &shape);
        let cost = cost_usage(&usage, &opus(), &shape);
        let alloc_sum: MicroUsd = allocs.iter().map(|a| a.micros).sum();
        assert_eq!(alloc_sum, cost.total);
        // System should dominate fresh input for the bloated case.
        let sys: u64 = allocs
            .iter()
            .filter(|a| a.component == Component::System && a.class == CacheClass::Fresh)
            .map(|a| a.tokens)
            .sum();
        let user: u64 = allocs
            .iter()
            .filter(|a| a.component == Component::UserMessage)
            .map(|a| a.tokens)
            .sum();
        assert!(sys > user, "system {sys} should dominate user {user}");
    }

    #[test]
    fn alloc_micros_sum_to_cost_with_audio() {
        // With NON-ZERO audio rates, allocate_step must emit audio leaves so the allocations still
        // sum to the report total (review 2026-07-03 — previously they were dropped).
        let table = crate::pricing::PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":5000000,
               "output_micro_per_mtok":25000000,"cache_read_micro_per_mtok":500000,
               "cache_write_5m_micro_per_mtok":6250000,"cache_write_1h_micro_per_mtok":10000000,
               "audio_input_micro_per_mtok":8000000,"audio_output_micro_per_mtok":16000000}]}"#,
        )
        .unwrap();
        let rates = table.find_by_model("m").unwrap();
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let (shape, mut usage, _) = wire::parse_step(Provider::Anthropic, req, resp).unwrap();
        usage.audio_input = 300;
        usage.audio_output = 200;
        let allocs = allocate_step(&usage, rates, &shape);
        let cost = cost_usage(&usage, rates, &shape);
        let alloc_sum: MicroUsd = allocs.iter().map(|a| a.micros).sum();
        assert_eq!(
            alloc_sum, cost.total,
            "audio leaves must keep parts == total"
        );
        assert!(
            allocs.iter().any(|a| a.tokens == 300),
            "audio-input leaf present"
        );
        assert!(
            allocs.iter().any(|a| a.tokens == 200),
            "audio-output leaf present"
        );
    }
}
