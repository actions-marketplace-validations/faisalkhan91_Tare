//! Prompt-cache advisor: the prescriptive flip of the `bloated-system-prompt` cause. For each
//! stable system prefix re-sent uncached, says how much caching it WOULD have saved over the
//! captured window (5m vs 1h tier) and the break-even read count. Pure, integer, deterministic.
//!
//! Framing is RETROSPECTIVE only: "you re-sent this N times; cached it would have cost
//! $X not $Y" — never a forward "$/day" projection (that would need a clock/extrapolation).

use crate::account::allocate_step;
use crate::model::{CacheClass, Component, Provider, RunRecord, StepRecord};
use crate::money::MicroUsd;
use crate::pricing::{ModelRates, PricingTable};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheAdvice {
    /// Pricing-provider identity (`anthropic`, `openai`, or an OpenAI-compatible vendor label).
    /// Kept alongside `model` because the same model id can have different rates on two backends.
    pub provider: String,
    pub model: String,
    /// Fresh system-prompt tokens in the repeated prefix (per send).
    pub system_tokens: u64,
    /// How many times the identical uncached prefix was sent in the window.
    pub sends: u32,
    /// What was actually paid sending it uncached every time.
    pub uncached_micros: i64,
    /// What it would have cost cached at each tier (1 write + (sends-1) reads).
    pub cached_5m_micros: i64,
    pub cached_1h_micros: i64,
    pub save_5m_micros: i64,
    pub save_1h_micros: i64,
    /// `"5m"`, `"1h"`, or `"none"` (no positive saving over this window).
    pub recommend: String,
    /// Reads needed to break even on the chosen tier's write premium (`0` if already ahead;
    /// `None` if a read is not cheaper than fresh for this rate row).
    pub breakeven_reads: Option<u32>,
}

fn system_fresh_tokens(step: &StepRecord, rates: &ModelRates) -> u64 {
    allocate_step(&step.usage, rates, &step.shape)
        .iter()
        .filter(|a| a.component == Component::System && a.class == CacheClass::Fresh)
        .map(|a| a.tokens)
        .fold(0u64, u64::saturating_add)
}

fn micros(tokens: u64, rate: i64) -> i64 {
    MicroUsd::for_tokens(tokens, rate).micros()
}

/// Hypothetical cache cost at one TTL. Timestamped sends form warm chains only while consecutive
/// accesses remain within the TTL. When timing is absent, assume at most one warm chain per run;
/// treating unrelated runs as one cache lifetime would overstate the saving.
pub(crate) fn hypothetical_cached_cost(
    steps: &[&StepRecord],
    tokens: u64,
    write_rate: i64,
    read_rate: i64,
    ttl_seconds: u64,
) -> i64 {
    let write_cost = micros(tokens, write_rate);
    let read_cost = micros(tokens, read_rate);
    let ttl_nanos = u128::from(ttl_seconds) * 1_000_000_000;
    let mut total = 0i64;

    let mut timed: Vec<(u128, &StepRecord)> = steps
        .iter()
        .filter_map(|step| step.start_unix_nano.map(|at| (at.get(), *step)))
        .collect();
    timed.sort_by(|(at_a, step_a), (at_b, step_b)| {
        at_a.cmp(at_b)
            .then(step_a.run_id.cmp(&step_b.run_id))
            .then(step_a.step_ordinal.cmp(&step_b.step_ordinal))
    });
    let mut previous = None;
    for (at, _) in timed {
        let warm = previous.is_some_and(|last: u128| at.saturating_sub(last) <= ttl_nanos);
        total = total.saturating_add(if warm { read_cost } else { write_cost });
        previous = Some(at);
    }

    let mut untimed_per_run: BTreeMap<&str, u64> = BTreeMap::new();
    for step in steps.iter().filter(|step| step.start_unix_nano.is_none()) {
        let count = untimed_per_run.entry(step.run_id.as_str()).or_default();
        *count = count.saturating_add(1);
    }
    for sends in untimed_per_run.into_values() {
        total = total.saturating_add(write_cost);
        let reads = i64::try_from(sends.saturating_sub(1)).unwrap_or(i64::MAX);
        total = total.saturating_add(read_cost.saturating_mul(reads));
    }
    total
}

/// Cache advice per repeated uncached system prefix. Only groups sent ≥2× without
/// `cache_control` and with a non-zero system prefix are advised. Sorted by 5m saving desc.
pub fn advise(runs: &[RunRecord], pricing: &PricingTable) -> Vec<CacheAdvice> {
    // Keep provider/vendor in the identity: the same model label can have different rates on two
    // backends, and merging them would price one backend with the other's row.
    let mut groups: BTreeMap<(Provider, Option<String>, String, u64), Vec<&StepRecord>> =
        BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            if step.shape.has_cache_control {
                continue;
            }
            let Some(hash) = step.shape.system_hash else {
                continue;
            };
            groups
                .entry((
                    step.provider,
                    step.shape.vendor.clone(),
                    step.model.clone(),
                    hash,
                ))
                .or_default()
                .push(step);
        }
    }

    let mut out = Vec::new();
    for ((provider, vendor, model, _hash), steps) in groups {
        if steps.len() < 2 {
            continue;
        }
        let Some(rates) = pricing.lookup(provider, vendor.as_deref(), &model) else {
            continue;
        };
        let sys_tok = system_fresh_tokens(steps[0], rates);
        if sys_tok == 0 {
            continue;
        }
        let n = u64::try_from(steps.len()).unwrap_or(u64::MAX);
        let f = rates.micro_per_mtok(CacheClass::Fresh);
        let r = rates.micro_per_mtok(CacheClass::CacheRead);
        let w5 = rates.micro_per_mtok(CacheClass::CacheWrite5m);
        let w1 = rates.micro_per_mtok(CacheClass::CacheWrite1h);

        let per_fresh = micros(sys_tok, f);
        let per_read = micros(sys_tok, r);
        let uncached = per_fresh.saturating_mul(i64::try_from(n).unwrap_or(i64::MAX));
        let cached_5m = hypothetical_cached_cost(&steps, sys_tok, w5, r, 5 * 60);
        let cached_1h = hypothetical_cached_cost(&steps, sys_tok, w1, r, 60 * 60);
        let save_5m = uncached.saturating_sub(cached_5m);
        let save_1h = uncached.saturating_sub(cached_1h);

        let (recommend, chosen_write_rate) = if save_5m <= 0 && save_1h <= 0 {
            ("none".to_string(), w5)
        } else if save_1h >= save_5m {
            ("1h".to_string(), w1)
        } else {
            ("5m".to_string(), w5)
        };

        // Break-even reads: write premium over fresh, divided by per-read saving vs fresh.
        let per_read_saving = per_fresh.saturating_sub(per_read); // f - r (>=0 normally)
        let write_premium = micros(sys_tok, chosen_write_rate).saturating_sub(per_fresh);
        let breakeven_reads = if per_read_saving <= 0 {
            None
        } else if write_premium <= 0 {
            Some(0)
        } else {
            // ceil(write_premium / per_read_saving)
            let reads = (i128::from(write_premium) + i128::from(per_read_saving) - 1)
                / i128::from(per_read_saving);
            Some(u32::try_from(reads).unwrap_or(u32::MAX))
        };

        out.push(CacheAdvice {
            provider: provider.pricing_key(vendor.as_deref()).into_owned(),
            model,
            system_tokens: sys_tok,
            sends: u32::try_from(n).unwrap_or(u32::MAX),
            uncached_micros: uncached,
            cached_5m_micros: cached_5m,
            cached_1h_micros: cached_1h,
            save_5m_micros: save_5m,
            save_1h_micros: save_1h,
            recommend,
            breakeven_reads,
        });
    }
    out.sort_by(|a, b| {
        b.save_5m_micros
            .max(b.save_1h_micros)
            .cmp(&a.save_5m_micros.max(a.save_1h_micros))
            .then(a.provider.cmp(&b.provider))
            .then(a.model.cmp(&b.model))
    });
    out
}

/// A cache breakpoint placed too early: the group DOES cache, but on the `tools`
/// block, leaving the stable `system` prefix stranded (re-billed fresh every send).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakpointAdvice {
    /// Pricing-provider identity; see [`CacheAdvice::provider`].
    pub provider: String,
    pub model: String,
    /// Stable `system` fresh tokens stranded AFTER the (too-early) breakpoint, per send.
    pub stranded_tokens: u64,
    /// How many times the group was sent (the breakpoint kept re-stranding system each time).
    pub sends: u32,
    pub save_5m_micros: i64,
    pub save_1h_micros: i64,
    /// `"5m"`, `"1h"`, or `"none"` — where to move/extend the breakpoint to.
    pub recommend: String,
}

/// Detect cache breakpoints placed TOO EARLY (the "multi-block breakpoint advisor").
/// The prompt prefix is ordered tools → system → messages; a single `cache_control` breakpoint
/// caches everything up to and including its block. The optimal placement is the last stable block
/// before the first volatile message — i.e. `system`. When the breakpoint instead sits on `tools`,
/// the (usually larger) `system` prefix is re-billed fresh on every send even though it is equally
/// stable; moving the breakpoint down to `system` caches it too.
///
/// Recoverable is `advise`'s own tier math applied to the STRANDED system tokens. Disjoint from:
///   - `advise` — it only considers `has_cache_control == false`; this only fires when `true`;
///   - `wasted_cache_write` — that needs `cache_read == 0`; this requires the cache to be HITTING
///     (`cache_read > 0`), so the two never claim the same prefix;
///   - `context_bloat_waste` — by regime: that fires on a cache that ERODED (was read, then went
///     fresh), while this fires on a cache that is still working but placed too early. The ledger's
///     capped-potential headline bounds any residual overlap by total spend regardless.
///
/// Pure, integer, deterministic.
pub fn suboptimal_breakpoint(runs: &[RunRecord], pricing: &PricingTable) -> Vec<BreakpointAdvice> {
    // Group by (model, system_hash) over the too-early shape: cache_control set, on the tools block.
    let mut groups: BTreeMap<(Provider, Option<String>, String, u64), Vec<&StepRecord>> =
        BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            if !step.shape.has_cache_control {
                continue; // advise() owns the uncached case
            }
            if step.shape.cached_component != Some(Component::Tools) {
                continue; // only the unambiguous "breakpoint on tools, system left fresh" shape
            }
            let Some(hash) = step.shape.system_hash else {
                continue;
            };
            groups
                .entry((
                    step.provider,
                    step.shape.vendor.clone(),
                    step.model.clone(),
                    hash,
                ))
                .or_default()
                .push(step);
        }
    }

    let mut out = Vec::new();
    for ((provider, vendor, model, _hash), steps) in groups {
        if steps.len() < 2 {
            continue;
        }
        // The cache must actually be hitting — otherwise this is `wasted_cache_write`'s case.
        let reads = steps
            .iter()
            .fold(0u64, |sum, step| sum.saturating_add(step.usage.cache_read));
        if reads == 0 {
            continue;
        }
        let Some(rates) = pricing.lookup(provider, vendor.as_deref(), &model) else {
            continue;
        };
        let stranded = system_fresh_tokens(steps[0], rates);
        if stranded == 0 {
            continue;
        }
        let n = i64::try_from(steps.len()).unwrap_or(i64::MAX);
        let f = rates.micro_per_mtok(CacheClass::Fresh);
        let r = rates.micro_per_mtok(CacheClass::CacheRead);
        let w5 = rates.micro_per_mtok(CacheClass::CacheWrite5m);
        let w1 = rates.micro_per_mtok(CacheClass::CacheWrite1h);
        // Don't assume the cache stays warm for all n-1 subsequent sends: if the observed (tools)
        // cache only hit intermittently (a gap cooled it), caching the stranded system prefix would
        // pay repeated re-WRITES, not reads — over-claiming the saving otherwise. Use the observed
        // warm-send count as the read count (capped at n-1: the first send is always a cold write),
        // and price the remaining sends as re-writes. All-warm reduces to the classic 1 write +
        // (n-1) reads; intermittent hits scale the saving down honestly.
        let warm = i64::try_from(steps.iter().filter(|s| s.usage.cache_read > 0).count())
            .unwrap_or(i64::MAX)
            .min(n.saturating_sub(1));
        let cold = n - warm; // sends that would re-write the stranded prefix (>= 1)
        let uncached = micros(stranded, f).saturating_mul(n);
        let reads_cost = micros(stranded, r).saturating_mul(warm);
        let cached = |w: i64| {
            micros(stranded, w)
                .saturating_mul(cold)
                .saturating_add(reads_cost)
        };
        let save_5m = uncached.saturating_sub(cached(w5));
        let save_1h = uncached.saturating_sub(cached(w1));
        let recommend = if save_5m <= 0 && save_1h <= 0 {
            "none".to_string()
        } else if save_1h >= save_5m {
            "1h".to_string()
        } else {
            "5m".to_string()
        };
        out.push(BreakpointAdvice {
            provider: provider.pricing_key(vendor.as_deref()).into_owned(),
            model,
            stranded_tokens: stranded,
            sends: u32::try_from(n).unwrap_or(u32::MAX),
            save_5m_micros: save_5m,
            save_1h_micros: save_1h,
            recommend,
        });
    }
    out.sort_by(|a, b| {
        b.save_5m_micros
            .cmp(&a.save_5m_micros)
            .then(a.provider.cmp(&b.provider))
            .then(a.model.cmp(&b.model))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;
    use crate::{build_runs, ingest_step};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    #[test]
    fn advises_caching_a_repeated_uncached_system_prefix() {
        // The bloated-system fixtures re-send the same large uncached system across 3 steps.
        let steps: Vec<_> = (1..=3)
            .map(|i| {
                let req = std::fs::read(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("..")
                        .join(format!(
                            "fixtures/bloated_system_prompt/step{i}.request.json"
                        )),
                )
                .unwrap();
                let resp = std::fs::read(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("..")
                        .join(format!(
                            "fixtures/bloated_system_prompt/step{i}.response.json"
                        )),
                )
                .unwrap();
                ingest_step("r", i, Provider::Anthropic, &req, &resp).unwrap()
            })
            .collect();
        let runs = build_runs(steps);
        let advice = advise(&runs, &pricing());
        assert_eq!(advice.len(), 1);
        let a = &advice[0];
        assert_eq!(a.sends, 3);
        assert!(a.system_tokens > 0);
        // Caching the stable prefix saves money, and the advisor recommends a tier.
        assert!(a.save_5m_micros > 0 || a.save_1h_micros > 0);
        assert_ne!(a.recommend, "none");
        assert!(a.breakeven_reads.is_some());
        // Cached cost is strictly below uncached.
        assert!(a.cached_5m_micros < a.uncached_micros);
    }

    #[test]
    fn single_send_is_not_advised() {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let step = ingest_step("r", 1, Provider::Openai, req, resp).unwrap();
        let runs = build_runs(vec![step]);
        assert!(advise(&runs, &pricing()).is_empty());
    }

    // ---- Suboptimal breakpoint (breakpoint on tools, system stranded) ----

    fn flat_pricing() -> PricingTable {
        // Opus-like flat rates (no context tiers), round numbers for exact-arithmetic assertions.
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":5000000,
               "output_micro_per_mtok":25000000,"cache_read_micro_per_mtok":500000,
               "cache_write_5m_micro_per_mtok":6250000,"cache_write_1h_micro_per_mtok":10000000}]}"#,
        )
        .unwrap()
    }

    /// A step whose breakpoint is on `tools` (cache hitting) while `system` is billed fresh.
    fn breakpoint_step(ord: u32, fresh_system: u64, cache_read: u64) -> crate::model::StepRecord {
        crate::model::StepRecord {
            run_id: "r".into(),
            step_ordinal: ord,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: crate::model::UsageTokens {
                fresh_input: fresh_system,
                cache_read,
                ..Default::default()
            },
            shape: crate::model::RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: true,
                cached_component: Some(Component::Tools),
                system_hash: Some(7),
                // All fresh_input attributes to System (the stranded, uncached stable prefix).
                weights: vec![crate::model::ComponentWeight {
                    component: Component::System,
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
        }
    }

    fn uncached_step(ord: u32, fresh_system: u64, at: Option<u128>) -> StepRecord {
        let mut step = breakpoint_step(ord, fresh_system, 0);
        step.shape.has_cache_control = false;
        step.shape.cached_component = None;
        step.start_unix_nano = at.map(crate::model::UnixNanos);
        step
    }

    #[test]
    fn cache_projection_respects_ttl_gaps() {
        let ten_minutes = 10u128 * 60 * 1_000_000_000;
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                uncached_step(1, 2000, Some(0)),
                uncached_step(2, 2000, Some(ten_minutes)),
                uncached_step(3, 2000, Some(ten_minutes * 2)),
            ],
        }];
        let out = advise(&runs, &flat_pricing());
        assert_eq!(out.len(), 1);
        // Every 5m entry is cold: 30_000 uncached − 37_500 cached. All three fit in a 1h chain:
        // 30_000 − (20_000 + 2×1_000) = 8_000.
        assert_eq!(out[0].save_5m_micros, -7_500);
        assert_eq!(out[0].save_1h_micros, 8_000);
        assert_eq!(out[0].recommend, "1h");
    }

    #[test]
    fn identical_prefixes_on_different_providers_are_not_merged() {
        let pricing = PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":5000000,
               "output_micro_per_mtok":25000000,"cache_read_micro_per_mtok":500000,
               "cache_write_5m_micro_per_mtok":6250000,"cache_write_1h_micro_per_mtok":10000000},
              {"provider":"openai","model_id":"m","input_micro_per_mtok":1000000,
               "output_micro_per_mtok":2000000,"cache_read_micro_per_mtok":100000,
               "cache_write_5m_micro_per_mtok":1250000,"cache_write_1h_micro_per_mtok":2000000}
            ]}"#,
        )
        .unwrap();
        let anthropic = uncached_step(1, 2000, None);
        let anthropic_2 = uncached_step(2, 2000, None);
        let mut openai = uncached_step(1, 2000, None);
        let mut openai_2 = uncached_step(2, 2000, None);
        for step in [&mut openai, &mut openai_2] {
            step.run_id = "other".into();
            step.provider = Provider::Openai;
            step.shape.provider = Provider::Openai;
        }
        let runs = vec![
            RunRecord {
                run_id: "r".into(),
                steps: vec![anthropic, anthropic_2],
            },
            RunRecord {
                run_id: "other".into(),
                steps: vec![openai, openai_2],
            },
        ];
        let out = advise(&runs, &pricing);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.iter().map(|a| a.provider.as_str()).collect::<Vec<_>>(),
            vec!["anthropic", "openai"]
        );
    }

    #[test]
    fn flags_a_breakpoint_stranding_the_system_prefix() {
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                breakpoint_step(1, 2000, 1000),
                breakpoint_step(2, 2000, 1000),
                breakpoint_step(3, 2000, 1000),
            ],
        }];
        let out = suboptimal_breakpoint(&runs, &flat_pricing());
        assert_eq!(out.len(), 1);
        let b = &out[0];
        assert_eq!(b.sends, 3);
        assert_eq!(b.stranded_tokens, 2000);
        // uncached = micros(2000,5e6)*3 = 30_000; reads = micros(2000,5e5)*2 = 2_000.
        // save_5m = 30_000 − (12_500 + 2_000) = 15_500; save_1h = 30_000 − (20_000 + 2_000) = 8_000.
        assert_eq!(b.save_5m_micros, 15_500);
        assert_eq!(b.save_1h_micros, 8_000);
        assert_eq!(b.recommend, "5m");
    }

    #[test]
    fn breakpoint_saving_scales_down_with_intermittent_cache_hits() {
        // Only the middle send hit the cache — caching the stranded system would re-write twice,
        // not read (n-1) times. The saving must reflect that, not the all-warm assumption.
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                breakpoint_step(1, 2000, 0),
                breakpoint_step(2, 2000, 1000),
                breakpoint_step(3, 2000, 0),
            ],
        }];
        let out = suboptimal_breakpoint(&runs, &flat_pricing());
        assert_eq!(out.len(), 1);
        // warm=1, cold=2: cached_5m = micros(2000,6.25e6)*2 + micros(2000,5e5)*1 = 25_000 + 1_000;
        // uncached = micros(2000,5e6)*3 = 30_000 → save_5m = 4_000 (vs 15_500 if assumed all-warm).
        assert_eq!(out[0].save_5m_micros, 4_000);
    }

    #[test]
    fn breakpoint_detector_respects_its_guards() {
        let pr = flat_pricing();
        // Single send -> not grouped.
        let one = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![breakpoint_step(1, 2000, 1000)],
        }];
        assert!(suboptimal_breakpoint(&one, &pr).is_empty(), "n<2 -> no row");

        // Cache never hit (reads==0) -> that's wasted_cache_write's case, not ours.
        let cold = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![breakpoint_step(1, 2000, 0), breakpoint_step(2, 2000, 0)],
        }];
        assert!(
            suboptimal_breakpoint(&cold, &pr).is_empty(),
            "reads==0 -> ceded to wasted_cache_write"
        );

        // Breakpoint already on System (optimal) -> nothing stranded -> no row.
        let mut opt = breakpoint_step(1, 2000, 1000);
        opt.shape.cached_component = Some(Component::System);
        let mut opt2 = opt.clone();
        opt2.step_ordinal = 2;
        let optimal = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![opt, opt2],
        }];
        assert!(
            suboptimal_breakpoint(&optimal, &pr).is_empty(),
            "breakpoint on system -> optimal -> no row"
        );
    }
}
