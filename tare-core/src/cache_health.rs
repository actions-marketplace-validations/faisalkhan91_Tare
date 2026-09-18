//! Cache-economics anti-pattern hunter: a first-class "cache health" lens that names
//! the specific prompt-caching anti-pattern per prompt-template + its $ impact — the on-device,
//! per-request view cloud dashboards can't produce. Complements the scattered savings/advise
//! surfaces with one classification pass.
//!
//! Per template (a stable-prefix `system_hash`) it classifies the dominant anti-pattern, each as one
//! flat line (no wrapping) so the doc list stays clippy-clean:
//!
//! single-use-write: written to cache (paying the 1.25×/2× premium) but never read back — MEASURED loss (the premium paid).
//! uncached-repeated: a stable prefix re-sent >=2x with no cache_control — PROJECTED saving of caching it (advise tier math).
//! low-read-ratio: cached + hitting, but reads are a small share of cache traffic — diagnostic (the ratio is the signal, no hard $).
//!
//! Plus a session lens, volatile-prefix: cache_control sits on content that CHANGES every send
//! (distinct prefixes ≈ cached sends, zero reads), so every "cached" send is a cold write never
//! reused. Its writes are the same ones the per-template pass counts as single-use-write, re-grouped
//! by session to explain WHY — not added to the loss total.
//!
//! Pure, integer micro-USD, counts-only, deterministic. Every figure is an estimate; measured losses
//! are premiums actually paid, projected figures carry the advise tokenizer/timing caveat. Captured
//! nanosecond timestamps bound TTL chains; timestamp-free data falls back conservatively per run.

use crate::account::allocate_step;
use crate::advise::hypothetical_cached_cost;
use crate::model::{CacheClass, Component, Provider, RunRecord, StepRecord};
use crate::money::MicroUsd;
use crate::pricing::{ModelRates, PricingTable};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Below this cache read-ratio (%), a cached template's writes aren't amortizing well.
const LOW_READ_RATIO_PCT: i64 = 50;

/// One prompt-template's cache health.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateHealth {
    pub model: String,
    /// Template label: `prefix#<system_hash>` when the stable-prefix fingerprint is present (proxy
    /// capture), else `session:<id>` (backfill/OTel data has cache counts but no prefix hash).
    pub template: String,
    pub sends: u32,
    pub read_tokens: u64,
    pub write_tokens: u64,
    /// `read × 100 / (read + write)`; `-1` when there was no cache activity at all.
    pub read_ratio_pct: i64,
    /// `single-use-write` | `uncached-repeated` | `low-read-ratio` | `healthy`.
    pub antipattern: String,
    /// Micro-USD impact of the anti-pattern (see `impact_kind`); 0 for diagnostics/healthy.
    pub impact_micros: i64,
    /// `lost` (measured premium paid) | `saveable` (projected) | `none`.
    pub impact_kind: String,
    /// This template's writes are on a volatile-prefix session (see the session lens).
    pub volatile_session: bool,
}

/// A session whose cache_control breakpoint sits on volatile content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolatileSession {
    pub session: String,
    /// Cached sends whose prefix never repeated.
    pub cached_sends: u32,
    pub distinct_prefixes: u32,
    /// Write premium paid on the volatile prefixes (never reused). A re-view of the per-template
    /// single-use-write loss grouped by session — NOT added to `total_lost_micros`.
    pub lost_micros: i64,
}

/// The cache-health report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHealthReport {
    /// Templates with an anti-pattern, ranked by |impact| desc (healthy templates omitted).
    pub flagged: Vec<TemplateHealth>,
    pub volatile_sessions: Vec<VolatileSession>,
    /// Σ measured single-use-write premium (the authoritative loss headline).
    pub total_lost_micros: i64,
    /// Σ projected uncached-repeated saving.
    pub total_saveable_micros: i64,
    /// Templates examined (including healthy).
    pub templates_examined: u32,
    pub pricing_version: String,
}

fn micros(tokens: u64, rate: i64) -> i64 {
    MicroUsd::for_tokens(tokens, rate).micros()
}

fn system_fresh_tokens(step: &StepRecord, rates: &ModelRates) -> u64 {
    allocate_step(&step.usage, rates, &step.shape)
        .iter()
        .filter(|a| a.component == Component::System && a.class == CacheClass::Fresh)
        .map(|a| a.tokens)
        .fold(0u64, u64::saturating_add)
}

/// The write premium actually paid on a step's cache writes (cost over just sending them fresh).
fn write_premium(step: &StepRecord, rates: &ModelRates) -> i64 {
    let w5 = step.usage.cache_write_5m;
    let w1 = step.usage.cache_write_1h;
    let write_cost = micros(w5, rates.micro_per_mtok(CacheClass::CacheWrite5m))
        .saturating_add(micros(w1, rates.micro_per_mtok(CacheClass::CacheWrite1h)));
    let fresh_equiv = micros(
        w5.saturating_add(w1),
        rates.micro_per_mtok(CacheClass::Fresh),
    );
    write_cost.saturating_sub(fresh_equiv).max(0)
}

struct Group<'a> {
    model: String,
    label: String,
    steps: Vec<&'a StepRecord>,
    rates: &'a ModelRates,
}

/// Template label: the stable-prefix fingerprint if present (proxy), else the session (backfill/OTel
/// have cache counts but no prefix hash). `None` when neither exists (can't group).
fn template_label(step: &StepRecord) -> Option<String> {
    if let Some(h) = step.shape.system_hash {
        Some(format!("prefix#{h}"))
    } else {
        step.shape.session.as_ref().map(|s| format!("session:{s}"))
    }
}

/// Classify each prompt-template's cache anti-pattern with its $ impact. Pure + deterministic.
pub fn cache_health(runs: &[RunRecord], pricing: &PricingTable) -> CacheHealthReport {
    // Keep provider/vendor in the identity so an identical model label on two backends never
    // inherits whichever rate row happened to be inserted first.
    let mut groups: BTreeMap<(Provider, Option<String>, String, String), Group> = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            let Some(label) = template_label(step) else {
                continue;
            };
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue; // unpriced → not a cache-$ finding (surfaced elsewhere as a gap)
            };
            groups
                .entry((
                    step.provider,
                    step.shape.vendor.clone(),
                    step.model.clone(),
                    label.clone(),
                ))
                .or_insert_with(|| Group {
                    model: step.model.clone(),
                    label,
                    steps: Vec::new(),
                    rates,
                })
                .steps
                .push(step);
        }
    }

    // Session lens: which sessions run cache_control over a volatile (ever-changing) prefix?
    // Per session: distinct cached prefixes, cached-send count, reads, and write premium.
    struct Sess {
        cached_hashes: BTreeSet<u64>,
        cached_sends: u32,
        reads: u64,
        premium: i64,
    }
    let mut sessions: BTreeMap<String, Sess> = BTreeMap::new();
    for run in runs {
        for step in &run.steps {
            if !step.shape.has_cache_control {
                continue;
            }
            let (Some(hash), Some(session)) = (step.shape.system_hash, step.shape.session.as_ref())
            else {
                continue;
            };
            let Some(rates) =
                pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
            else {
                continue;
            };
            let e = sessions.entry(session.clone()).or_insert_with(|| Sess {
                cached_hashes: BTreeSet::new(),
                cached_sends: 0,
                reads: 0,
                premium: 0,
            });
            e.cached_hashes.insert(hash);
            e.cached_sends = e.cached_sends.saturating_add(1);
            e.reads = e.reads.saturating_add(step.usage.cache_read);
            e.premium = e.premium.saturating_add(write_premium(step, rates));
        }
    }
    let volatile_sessions: Vec<VolatileSession> = sessions
        .into_iter()
        .filter(|(_, s)| {
            // Every cached send a distinct prefix, ≥2 sends, and nothing ever read back.
            let distinct = u32::try_from(s.cached_hashes.len()).unwrap_or(u32::MAX);
            s.cached_sends >= 2 && distinct == s.cached_sends && s.reads == 0 && s.premium > 0
        })
        .map(|(session, s)| VolatileSession {
            session,
            cached_sends: s.cached_sends,
            distinct_prefixes: u32::try_from(s.cached_hashes.len()).unwrap_or(u32::MAX),
            lost_micros: s.premium,
        })
        .collect();
    let volatile_session_ids: BTreeSet<String> = volatile_sessions
        .iter()
        .map(|v| v.session.clone())
        .collect();

    let mut flagged = Vec::new();
    let mut total_lost = 0i64;
    let mut total_saveable = 0i64;
    let mut examined = 0u32;
    for g in groups.values() {
        examined = examined.saturating_add(1);
        let sends = u32::try_from(g.steps.len()).unwrap_or(u32::MAX);
        let reads = g
            .steps
            .iter()
            .fold(0u64, |sum, step| sum.saturating_add(step.usage.cache_read));
        let writes = g.steps.iter().fold(0u64, |sum, step| {
            sum.saturating_add(step.usage.cache_write())
        });
        let has_cc = g.steps.iter().any(|s| s.shape.has_cache_control);
        let f = g.rates.micro_per_mtok(CacheClass::Fresh);
        let r = g.rates.micro_per_mtok(CacheClass::CacheRead);
        let w5 = g.rates.micro_per_mtok(CacheClass::CacheWrite5m);
        let w1 = g.rates.micro_per_mtok(CacheClass::CacheWrite1h);
        let cache_traffic = u128::from(reads) + u128::from(writes);
        let read_ratio_pct = (u128::from(reads) * 100)
            .checked_div(cache_traffic)
            .map(|ratio| ratio as i64)
            .unwrap_or(-1);
        let volatile_session = g.steps.iter().any(|s| {
            s.shape
                .session
                .as_ref()
                .is_some_and(|sid| volatile_session_ids.contains(sid))
        });

        let (antipattern, impact_micros, impact_kind) = if writes > 0 && reads == 0 {
            // single-use-write: the write premium is pure loss (paid to cache, never read).
            let lost = g.steps.iter().fold(0i64, |sum, step| {
                sum.saturating_add(write_premium(step, g.rates))
            });
            total_lost = total_lost.saturating_add(lost);
            ("single-use-write", lost, "lost")
        } else if !has_cc && sends >= 2 {
            // uncached-repeated: caching the stable prefix would save (projected, advise math).
            let fresh = system_fresh_tokens(g.steps[0], g.rates);
            if fresh == 0 {
                ("healthy", 0, "none")
            } else {
                let n = i64::from(sends);
                let uncached = micros(fresh, f).saturating_mul(n);
                let save5 = uncached.saturating_sub(hypothetical_cached_cost(
                    &g.steps,
                    fresh,
                    w5,
                    r,
                    5 * 60,
                ));
                let save1 = uncached.saturating_sub(hypothetical_cached_cost(
                    &g.steps,
                    fresh,
                    w1,
                    r,
                    60 * 60,
                ));
                let save = save5.max(save1).max(0);
                if save > 0 {
                    total_saveable = total_saveable.saturating_add(save);
                    ("uncached-repeated", save, "saveable")
                } else {
                    ("healthy", 0, "none")
                }
            }
        } else if has_cc && reads > 0 && writes > 0 && read_ratio_pct < LOW_READ_RATIO_PCT {
            ("low-read-ratio", 0, "none") // diagnostic: the ratio is the signal
        } else {
            ("healthy", 0, "none")
        };

        if antipattern != "healthy" {
            flagged.push(TemplateHealth {
                model: g.model.clone(),
                template: g.label.clone(),
                sends,
                read_tokens: reads,
                write_tokens: writes,
                read_ratio_pct,
                antipattern: antipattern.to_string(),
                impact_micros,
                impact_kind: impact_kind.to_string(),
                volatile_session,
            });
        }
    }

    flagged.sort_by(|a, b| {
        b.impact_micros
            .cmp(&a.impact_micros)
            .then(a.model.cmp(&b.model))
            .then(a.template.cmp(&b.template))
    });

    CacheHealthReport {
        flagged,
        volatile_sessions,
        total_lost_micros: total_lost,
        total_saveable_micros: total_saveable,
        templates_examined: examined,
        pricing_version: pricing.version.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheTtl, ComponentWeight, Provider, RequestShape, RunRecord, UnixNanos, UsageTokens,
    };

    fn flat_pricing() -> PricingTable {
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":5000000,
               "output_micro_per_mtok":25000000,"cache_read_micro_per_mtok":500000,
               "cache_write_5m_micro_per_mtok":6250000,"cache_write_1h_micro_per_mtok":10000000}]}"#,
        )
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn step(
        ord: u32,
        hash: u64,
        session: &str,
        has_cc: bool,
        fresh: u64,
        cw5m: u64,
        read: u64,
    ) -> StepRecord {
        StepRecord {
            run_id: "r".into(),
            step_ordinal: ord,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: UsageTokens {
                fresh_input: fresh,
                cache_write_5m: cw5m,
                cache_read: read,
                ..Default::default()
            },
            shape: RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
                has_cache_control: has_cc,
                cached_component: None,
                system_hash: Some(hash),
                weights: vec![ComponentWeight {
                    component: Component::System,
                    bytes: 1,
                }],
                request_hash: Some(ord as u64),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: Some(session.into()),
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

    fn runs(steps: Vec<StepRecord>) -> Vec<RunRecord> {
        vec![RunRecord {
            run_id: "r".into(),
            steps,
        }]
    }

    #[test]
    fn single_use_write_is_measured_loss() {
        // One prefix written (2000 tok @ 5m) but never read → premium is pure loss.
        let r = cache_health(
            &runs(vec![step(1, 7, "s", true, 0, 2000, 0)]),
            &flat_pricing(),
        );
        let t = r
            .flagged
            .iter()
            .find(|t| t.antipattern == "single-use-write")
            .unwrap();
        // premium = micros(2000,6.25e6) − micros(2000,5e6) = 12_500 − 10_000 = 2_500.
        assert_eq!(t.impact_micros, 2_500);
        assert_eq!(t.impact_kind, "lost");
        assert_eq!(r.total_lost_micros, 2_500);
    }

    #[test]
    fn uncached_repeated_is_projected_saving() {
        // Same stable prefix (2000 fresh system tok) sent 3× uncached → should be cached.
        let s = |o| step(o, 9, "s", false, 2000, 0, 0);
        let r = cache_health(&runs(vec![s(1), s(2), s(3)]), &flat_pricing());
        let t = r
            .flagged
            .iter()
            .find(|t| t.antipattern == "uncached-repeated")
            .unwrap();
        // save_5m = 3*10_000 − (12_500 + 2*1_000) = 30_000 − 14_500 = 15_500.
        assert_eq!(t.impact_micros, 15_500);
        assert_eq!(t.impact_kind, "saveable");
        assert_eq!(r.total_saveable_micros, 15_500);
    }

    #[test]
    fn uncached_projection_uses_captured_timing() {
        let ten_minutes = 10u128 * 60 * 1_000_000_000;
        let mut steps = vec![
            step(1, 9, "s", false, 2000, 0, 0),
            step(2, 9, "s", false, 2000, 0, 0),
            step(3, 9, "s", false, 2000, 0, 0),
        ];
        for (index, item) in steps.iter_mut().enumerate() {
            item.start_unix_nano = Some(UnixNanos(u128::try_from(index).unwrap() * ten_minutes));
        }
        let report = cache_health(&runs(steps), &flat_pricing());
        let finding = report
            .flagged
            .iter()
            .find(|item| item.antipattern == "uncached-repeated")
            .unwrap();
        assert_eq!(finding.impact_micros, 8_000); // 1h wins; every 5m access is cold
    }

    #[test]
    fn same_model_and_prefix_on_different_providers_stay_separate() {
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
        let anthropic = step(1, 9, "a", false, 2000, 0, 0);
        let mut openai = step(1, 9, "b", false, 2000, 0, 0);
        openai.run_id = "other".into();
        openai.provider = Provider::Openai;
        openai.shape.provider = Provider::Openai;
        let report = cache_health(
            &[
                RunRecord {
                    run_id: "r".into(),
                    steps: vec![anthropic],
                },
                RunRecord {
                    run_id: "other".into(),
                    steps: vec![openai],
                },
            ],
            &pricing,
        );
        assert_eq!(report.templates_examined, 2);
        assert!(report.flagged.is_empty());
    }

    #[test]
    fn volatile_prefix_session_is_flagged() {
        // A session that caches a DIFFERENT prefix every send (never reads) → breakpoint on volatile
        // content. Three distinct hashes, all cached writes, zero reads.
        let r = cache_health(
            &runs(vec![
                step(1, 100, "vol", true, 0, 2000, 0),
                step(2, 200, "vol", true, 0, 2000, 0),
                step(3, 300, "vol", true, 0, 2000, 0),
            ]),
            &flat_pricing(),
        );
        assert_eq!(r.volatile_sessions.len(), 1);
        let v = &r.volatile_sessions[0];
        assert_eq!(v.cached_sends, 3);
        assert_eq!(v.distinct_prefixes, 3);
        assert_eq!(v.lost_micros, 7_500); // 3 × 2_500 premium
                                          // Each template is also flagged single-use-write with the volatile-session marker set.
        assert!(r
            .flagged
            .iter()
            .all(|t| t.antipattern == "single-use-write" && t.volatile_session));
        assert_eq!(r.total_lost_micros, 7_500); // per-template loss (session lens re-views it)
    }
}
