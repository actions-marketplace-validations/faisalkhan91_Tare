//! Advisories are non-floor "things to look at." Each advisory carries an
//! AT-RISK or UPPER-BOUND figure that is deliberately **NOT** summed into the [`crate::savings`]
//! ledger floor, because an honest recoverable-dollar cannot be claimed:
//!   - **batch** — the Batch API's 0.5× is multiplicative over the *same* tokens every other engine
//!     reprices, and eligibility (latency-tolerance, up to 24h turnaround) is *not observed* from a
//!     non-streaming flag alone. Summing it would double-count ~half of nearly all spend.
//!   - **reasoning-effort** — there is no priced "effort tier" (effort changes token *count*, not
//!     rate) and no counterfactual for how many reasoning tokens a lower tier would emit, so any
//!     recoverable fraction would be fabricated. We surface the reasoning $ at risk, not a saving.
//!   - **compression** — realizing it needs an *external* compressor (LLMLingua's 2–5× is a
//!     model-dependent range, never a guaranteed $), so we present a labelled ratio band.
//!   - **cache-scorecard** — pure diagnostics (read/write ratio, below-minimum cache_control).
//!
//! Surfaced separately from the recoverable headline so that number stays defensible. Pure,
//! integer micro-USD, clock-free, deterministic.

use crate::account::{allocate_step, cost_from_usage_scaled, cost_usage, PriceScalars};
use crate::model::{CacheClass, Component, RunRecord, StepRecord};
use crate::money::MicroUsd;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A reasoning answer shorter than this (output − reasoning tokens) counts as "short/clean".
const SIMPLE_OUTPUT_MAX: u64 = 500;
/// Reasoning must be at least this multiple of the answer to look disproportionate.
const REASON_RATIO: u64 = 4;
/// Compression is only sized on aggregates at least this large (fixed overhead + quality risk).
const MIN_COMPRESSIBLE_TOKENS: u64 = 2048;
/// Both compression and the too-early-breakpoint story need recurrence to matter.
const RECUR_MIN: usize = 2;

/// One non-floor advisory. `at_risk_micros` is an exposed / upper-bound figure, NEVER a recoverable
/// dollar — it is intentionally absent from [`crate::savings::SavingsLedger`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advisory {
    /// `batch` | `reasoning-effort` | `compression` | `cache-scorecard`.
    pub kind: String,
    /// Model id or prefix identity the advisory is about.
    pub label: String,
    /// Micro-USD currently *exposed* to this pattern (an upper bound), not a claimed saving.
    pub at_risk_micros: i64,
    /// One-line summary.
    pub headline: String,
    /// Longer explanation, including any labelled band / scorecard numbers.
    pub detail: String,
}

/// A stop reason that indicates the response finished cleanly (not truncated / errored / refused).
fn is_clean_stop(sr: &Option<String>) -> bool {
    match sr.as_deref() {
        None => true,
        Some(s) => matches!(
            s,
            "end_turn" | "stop" | "stop_sequence" | "tool_use" | "end" | "complete"
        ),
    }
}

/// Fresh tokens on *compressible* (variable, non-cacheable-prefix) components: user/assistant
/// messages, tool results, and the catch-all — never System/Tools (those are the cache's job) and
/// never Output. By byte-weight apportionment, so it is an estimate (kept out of the floor).
fn compressible_fresh_tokens(step: &StepRecord, rates: &crate::pricing::ModelRates) -> u64 {
    allocate_step(&step.usage, rates, &step.shape)
        .into_iter()
        .filter(|a| {
            a.class == CacheClass::Fresh
                && matches!(
                    a.component,
                    Component::UserMessage
                        | Component::AssistantMessage
                        | Component::ToolResult
                        | Component::Other
                )
        })
        .map(|a| a.tokens)
        .fold(0u64, u64::saturating_add)
}

/// Build the advisory list across all detectors. Pure function of stored steps and
/// pricing. Ordered by `at_risk_micros` desc, then kind, then label (deterministic).
pub fn advisories(runs: &[RunRecord], pricing: &PricingTable) -> Vec<Advisory> {
    let mut out = Vec::new();
    out.extend(batch_eligibility(runs, pricing));
    out.extend(reasoning_effort(runs, pricing));
    out.extend(compression_sizer(runs, pricing));
    out.extend(cache_scorecard(runs, pricing));
    out.sort_by(|a, b| {
        b.at_risk_micros
            .cmp(&a.at_risk_micros)
            .then(a.kind.cmp(&b.kind))
            .then(a.label.cmp(&b.label))
    });
    out
}

/// Non-streaming (`stream == false`) requests may use the Batch API for 0.5× on
/// input+output — IF the workload tolerates up to 24h latency, which a sync non-streaming call in an
/// interactive agent does not establish. Advisory only: the 0.5× is not drained into the floor.
fn batch_eligibility(runs: &[RunRecord], pricing: &PricingTable) -> Vec<Advisory> {
    let mut by_model: BTreeMap<String, (i64, u64)> = BTreeMap::new(); // model -> (at_risk, steps)
    for run in runs {
        for step in &run.steps {
            if step.shape.stream {
                continue;
            }
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            let full = cost_usage(&step.usage, rates, &step.shape).total.micros();
            let batched = cost_from_usage_scaled(&step.usage, rates, PriceScalars::batch())
                .total
                .micros();
            let delta = full.saturating_sub(batched);
            if delta > 0 {
                let e = by_model.entry(step.model.clone()).or_insert((0, 0));
                e.0 = e.0.saturating_add(delta);
                e.1 = e.1.saturating_add(1);
            }
        }
    }
    by_model
        .into_iter()
        .map(|(model, (at_risk, steps))| Advisory {
            kind: "batch".into(),
            label: model.clone(),
            at_risk_micros: at_risk,
            headline: format!(
                "{steps} non-streaming call(s) on `{model}` could cost ~50% less via the Batch API"
            ),
            detail:
                "The Batch API is 0.5× on input+output but turns around asynchronously (up to 24h). \
                 Only worth it if this workload is latency-tolerant — a synchronous non-streaming \
                 call in an interactive loop is NOT batch-eligible, so this is not counted as a saving."
                    .into(),
        })
        .collect()
}

/// High reasoning effort that produced a large reasoning trace for a short, clean answer
/// — a candidate for a lower effort tier. There is no priced effort tier and no counterfactual, so
/// we show the reasoning tokens *at risk*, never a recoverable fraction.
fn reasoning_effort(runs: &[RunRecord], pricing: &PricingTable) -> Vec<Advisory> {
    let mut by_key: BTreeMap<(String, String), (i64, u64)> = BTreeMap::new(); // (model,effort)->(risk,steps)
    for run in runs {
        for step in &run.steps {
            let Some(effort) = step.shape.effort.as_deref() else {
                continue;
            };
            if !matches!(effort, "high" | "xhigh" | "max") {
                continue;
            }
            if !is_clean_stop(&step.stop_reason) {
                continue; // a short answer that was truncated/errored isn't wasted reasoning
            }
            // Reasoning is a subset of output; cap defensively so malformed usage (reasoning >
            // output) can't overstate the output dollars actually billed.
            let reasoning = step.usage.reasoning.min(step.usage.output);
            let answer = step.usage.output.saturating_sub(reasoning);
            if reasoning == 0 || answer >= SIMPLE_OUTPUT_MAX || reasoning < answer * REASON_RATIO {
                continue;
            }
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            let risk =
                MicroUsd::for_tokens(reasoning, rates.micro_per_mtok(CacheClass::Output)).micros();
            let e = by_key
                .entry((step.model.clone(), effort.to_string()))
                .or_insert((0, 0));
            e.0 = e.0.saturating_add(risk);
            e.1 = e.1.saturating_add(1);
        }
    }
    by_key
        .into_iter()
        .filter(|(_, (risk, _))| *risk > 0)
        .map(|((model, effort), (risk, steps))| Advisory {
            kind: "reasoning-effort".into(),
            label: format!("{model} @ {effort}"),
            at_risk_micros: risk,
            headline: format!(
                "{steps} step(s) at effort `{effort}` spent a large reasoning trace on a <{SIMPLE_OUTPUT_MAX}-token answer"
            ),
            detail: "Try a lower effort tier for this workload. The figure is the reasoning tokens' \
                 output cost at risk — not a guaranteed saving, since some reasoning is needed for \
                 the answer and effort changes token count, not rate."
                .to_string(),
        })
        .collect()
}

/// Prompt-compression sizer. For recurring groups with real variable (non-cacheable)
/// fresh bulk, present the recoverable range at 2×/3×/5× compression as a LABELLED BAND requiring an
/// external compressor (LLMLingua) — never a guaranteed dollar. Redundant (loop) re-issues and
/// failed steps are excluded from the base; groups whose fresh is all cacheable prefix (owned by
/// `advise`/`breakpoint`) size to zero.
fn compression_sizer(runs: &[RunRecord], pricing: &PricingTable) -> Vec<Advisory> {
    // Per (model, system_hash): compressible_micros, compressible_tokens, sends, seen request hashes.
    type Group = (i64, u64, usize, BTreeSet<u64>);
    let mut groups: BTreeMap<(String, u64), Group> = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            if !is_clean_stop(&step.stop_reason) {
                continue; // failed step: don't resell its tokens as compressible
            }
            let Some(hash) = step.shape.system_hash else {
                continue;
            };
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            let e = groups
                .entry((step.model.clone(), hash))
                .or_insert((0, 0, 0, BTreeSet::new()));
            // Redundant loop re-issue (identical request) — exclude, it's loop waste not compression.
            if let Some(rh) = step.shape.request_hash {
                if !e.3.insert(rh) {
                    continue;
                }
            }
            let toks = compressible_fresh_tokens(step, rates);
            if toks == 0 {
                continue;
            }
            let micros =
                MicroUsd::for_tokens(toks, rates.micro_per_mtok(CacheClass::Fresh)).micros();
            e.0 = e.0.saturating_add(micros);
            e.1 = e.1.saturating_add(toks);
            e.2 = e.2.saturating_add(1);
        }
    }
    groups
        .into_iter()
        .filter(|(_, (micros, toks, sends, _))| {
            *sends >= RECUR_MIN && *toks >= MIN_COMPRESSIBLE_TOKENS && *micros > 0
        })
        .map(|((model, _hash), (micros, toks, sends, _))| {
            // recoverable at N× = micros × (N−1)/N (floor). Widen to i128 — `micros × (n-1)` on a
            // large aggregate would overflow i64 (panic in debug / wrap in release).
            let band = |n: i64| ((micros as i128 * (n as i128 - 1)) / n as i128) as i64;
            Advisory {
                kind: "compression".into(),
                label: model.clone(),
                at_risk_micros: micros,
                headline: format!(
                    "{toks} tokens of variable prompt bulk re-sent across {sends} calls on `{model}` — compressible"
                ),
                detail: format!(
                    "Upper-bound recovery with an external compressor (e.g. LLMLingua): \
                     2×≈{}µ$, 3×≈{}µ$, 5×≈{}µ$. This is a MODEL-DEPENDENT range, not a guaranteed \
                     saving, so it is not counted toward recoverable. Caching cannot help here — \
                     these are variable message/tool tokens, not a stable prefix.",
                    band(2),
                    band(3),
                    band(5)
                ),
            }
        })
        .collect()
}

/// Cache scorecard: pure diagnostics per prefix. Read/write ratio and a below-minimum
/// flag (`cache_control` set but the provider reported no cache activity → the prefix is below the
/// minimum cacheable size, so pad it or drop the marker). No dollars claimed (`at_risk = 0`).
fn cache_scorecard(runs: &[RunRecord], _pricing: &PricingTable) -> Vec<Advisory> {
    // A provider caches a prefix per model, so identical text hashes on different models must not
    // share read/write activity.
    let mut by_prefix: BTreeMap<(String, u64), (u64, u64, bool)> = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            let Some(hash) = step.shape.system_hash else {
                continue;
            };
            let e = by_prefix
                .entry((step.model.clone(), hash))
                .or_insert((0, 0, false));
            e.0 = e.0.saturating_add(step.usage.cache_read);
            e.1 = e.1.saturating_add(
                step.usage
                    .cache_write_5m
                    .saturating_add(step.usage.cache_write_1h),
            );
            e.2 |= step.shape.has_cache_control;
        }
    }
    let mut out = Vec::new();
    for ((model, hash), (reads, writes, has_cc)) in by_prefix {
        let label = format!("{model}/prefix#{hash}");
        // Below-minimum: cache_control set but nothing was ever written or read back.
        if has_cc && reads == 0 && writes == 0 {
            out.push(Advisory {
                kind: "cache-scorecard".into(),
                label: label.clone(),
                at_risk_micros: 0,
                headline: format!(
                    "`cache_control` set on {label} but no cache activity — below the minimum cacheable size"
                ),
                detail: "The provider caches only prefixes above a minimum (~1024 tokens; ~2048 on \
                         Haiku). Either pad the prefix to reach it or drop the `cache_control` marker."
                    .into(),
            });
            continue;
        }
        let activity = reads.saturating_add(writes);
        if activity == 0 {
            continue; // uncached prefix — advise()/breakpoint own that story
        }
        let ratio = (u128::from(reads) * 100 / u128::from(activity)) as u64;
        out.push(Advisory {
            kind: "cache-scorecard".into(),
            label: label.clone(),
            at_risk_micros: 0,
            headline: format!("Cache read-ratio {ratio}% on {label}"),
            detail: format!(
                "{reads} read tokens vs {writes} write tokens. A low read-ratio means writes aren't \
                 amortized — reuse the prefix more before its TTL, or don't cache it."
            ),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheTtl, ComponentWeight, Provider, RequestShape, RunRecord, StepRecord, UsageTokens,
    };

    fn flat_pricing() -> PricingTable {
        // Opus-like flat rates, round numbers for exact assertions.
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":5000000,
               "output_micro_per_mtok":25000000,"cache_read_micro_per_mtok":500000,
               "cache_write_5m_micro_per_mtok":6250000,"cache_write_1h_micro_per_mtok":10000000}]}"#,
        )
        .unwrap()
    }

    fn base_shape() -> RequestShape {
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

    #[test]
    fn batch_eligibility_halves_nonstreaming_cost_as_advisory() {
        let mut sh = base_shape();
        sh.stream = false;
        // 1000 fresh input = 5000µ$; 1000 output = 25000µ$; total 30000. Batch 0.5× -> 15000.
        let usage = UsageTokens {
            fresh_input: 1000,
            output: 1000,
            ..Default::default()
        };
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(1, usage, sh)],
        }];
        let adv = advisories(&runs, &flat_pricing());
        let b = adv
            .iter()
            .find(|a| a.kind == "batch")
            .expect("batch advisory");
        assert_eq!(b.at_risk_micros, 15_000);
        // A streaming step contributes nothing.
        let streaming = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(
                1,
                UsageTokens {
                    fresh_input: 1000,
                    output: 1000,
                    ..Default::default()
                },
                base_shape(),
            )],
        }];
        assert!(advisories(&streaming, &flat_pricing())
            .iter()
            .all(|a| a.kind != "batch"));
    }

    #[test]
    fn reasoning_effort_flags_big_trace_short_answer() {
        let mut sh = base_shape();
        sh.effort = Some("high".into());
        // output 10000, reasoning 9800 -> answer 200 (<500), reasoning >= 4*200. risk = 9800@25 = 245000.
        let usage = UsageTokens {
            output: 10_000,
            reasoning: 9_800,
            ..Default::default()
        };
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(1, usage, sh.clone())],
        }];
        let adv = advisories(&runs, &flat_pricing());
        let r = adv
            .iter()
            .find(|a| a.kind == "reasoning-effort")
            .expect("reasoning advisory");
        assert_eq!(r.at_risk_micros, 245_000);

        // effort=medium -> not flagged.
        let mut med = sh.clone();
        med.effort = Some("medium".into());
        let runs2 = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(
                1,
                UsageTokens {
                    output: 10_000,
                    reasoning: 9_800,
                    ..Default::default()
                },
                med,
            )],
        }];
        assert!(advisories(&runs2, &flat_pricing())
            .iter()
            .all(|a| a.kind != "reasoning-effort"));

        // Truncated short answer -> excluded (not wasted reasoning).
        let mut trunc = step(
            1,
            UsageTokens {
                output: 10_000,
                reasoning: 9_800,
                ..Default::default()
            },
            sh,
        );
        trunc.stop_reason = Some("max_tokens".into());
        let runs3 = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![trunc],
        }];
        assert!(advisories(&runs3, &flat_pricing())
            .iter()
            .all(|a| a.kind != "reasoning-effort"));
    }

    #[test]
    fn compression_sizes_recurring_variable_bulk_as_a_band() {
        let mut sh = base_shape();
        sh.system_hash = Some(1);
        // All fresh attributes to UserMessage (compressible), 2000 tokens/send -> 10000µ$/send.
        sh.weights = vec![ComponentWeight {
            component: Component::UserMessage,
            bytes: 1,
        }];
        let mk = |ord: u32| {
            let mut s = sh.clone();
            s.request_hash = Some(ord as u64); // distinct -> not redundant
            step(
                ord,
                UsageTokens {
                    fresh_input: 2000,
                    ..Default::default()
                },
                s,
            )
        };
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![mk(1), mk(2)],
        }];
        let adv = advisories(&runs, &flat_pricing());
        let c = adv
            .iter()
            .find(|a| a.kind == "compression")
            .expect("compression advisory");
        // agg compressible = 4000 tok = 20000µ$ >= MIN. 5× band = 20000*4/5 = 16000.
        assert_eq!(c.at_risk_micros, 20_000);
        assert!(c.detail.contains("16000"), "5x band shown: {}", c.detail);

        // A stable System prefix is NOT compressible (caching owns it) -> no row.
        let mut sys = base_shape();
        sys.system_hash = Some(2);
        sys.weights = vec![ComponentWeight {
            component: Component::System,
            bytes: 1,
        }];
        let sruns = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                step(
                    1,
                    UsageTokens {
                        fresh_input: 3000,
                        ..Default::default()
                    },
                    sys.clone(),
                ),
                step(
                    2,
                    UsageTokens {
                        fresh_input: 3000,
                        ..Default::default()
                    },
                    sys,
                ),
            ],
        }];
        assert!(advisories(&sruns, &flat_pricing())
            .iter()
            .all(|a| a.kind != "compression"));
    }

    #[test]
    fn cache_scorecard_reports_ratio_and_below_minimum() {
        // Below-minimum: cache_control set, no activity.
        let mut below = base_shape();
        below.system_hash = Some(9);
        below.has_cache_control = true;
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(1, UsageTokens::default(), below)],
        }];
        let adv = advisories(&runs, &flat_pricing());
        let s = adv
            .iter()
            .find(|a| a.kind == "cache-scorecard")
            .expect("scorecard");
        assert!(s.headline.contains("below the minimum"), "{}", s.headline);
        assert_eq!(s.at_risk_micros, 0);

        // Ratio: 800 read / (800+200 write) = 80%.
        let mut hit = base_shape();
        hit.system_hash = Some(10);
        let runs2 = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(
                1,
                UsageTokens {
                    cache_read: 800,
                    cache_write_5m: 200,
                    ..Default::default()
                },
                hit,
            )],
        }];
        let adv2 = advisories(&runs2, &flat_pricing());
        let s2 = adv2
            .iter()
            .find(|a| a.kind == "cache-scorecard")
            .expect("scorecard");
        assert!(s2.headline.contains("80%"), "{}", s2.headline);

        let mut extreme = base_shape();
        extreme.system_hash = Some(11);
        let extreme_runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step(
                1,
                UsageTokens {
                    cache_read: u64::MAX,
                    ..Default::default()
                },
                extreme,
            )],
        }];
        let extreme_score = advisories(&extreme_runs, &flat_pricing())
            .into_iter()
            .find(|a| a.kind == "cache-scorecard")
            .unwrap();
        assert!(extreme_score.headline.contains("100%"));
    }

    #[test]
    fn cache_scorecard_does_not_merge_identical_prefixes_across_models() {
        let mut a = base_shape();
        a.system_hash = Some(99);
        let mut b = a.clone();
        b.model = "other".into();
        let mut write = step(
            1,
            UsageTokens {
                cache_write_5m: 100,
                ..Default::default()
            },
            a,
        );
        let mut read = step(
            2,
            UsageTokens {
                cache_read: 100,
                ..Default::default()
            },
            b,
        );
        read.model = "other".into();
        write.model = "m".into();
        let rows = cache_scorecard(
            &[RunRecord {
                run_id: "r".into(),
                steps: vec![write, read],
            }],
            &flat_pricing(),
        );
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|r| r.headline.contains("0%")));
        assert!(rows.iter().any(|r| r.headline.contains("100%")));
    }
}
