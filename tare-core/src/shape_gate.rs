//! Deterministic cost-&-SHAPE regression assertions for `tare gate` (A3). A payload-free,
//! integer, clock-free structural check meant to run BEFORE expensive LLM-judge evals: it
//! answers "did the *shape* of my requests regress?" — system prompt ballooned, tool defs
//! grew, caching stopped paying off, a retry loop crept in — straight from the captured token
//! vectors.
//!
//! Token counts come from `account::allocate_step` (per-(Component, CacheClass) TOKEN integers),
//! never from `shape.weights` (which carries BYTES). Ratios are integer cross-multiplication
//! Retry detection is request-hash based and therefore unavailable under `max_private`
//! (the hash is dropped); that case is reported and fails closed at the gate, never silently
//! passes.

use crate::account::{allocate_step, allocate_unpriced_step};
use crate::model::{CacheClass, Component, RunRecord};
use crate::pricing::PricingTable;
use std::collections::{BTreeMap, BTreeSet};

/// Token-shape of a captured run set, aggregated deterministically.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapeStats {
    /// Total tokens attributed to each structural component across all steps (every cache class).
    pub per_component_tokens: BTreeMap<Component, u64>,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// False when ANY step lacks a `request_hash` (e.g. the `max_private` profile drops it):
    /// retry detection cannot be trusted, so a `--no-retry-loops` assertion must fail closed.
    pub retry_available: bool,
    /// True when some run re-issued an identical `request_hash` (a redundant re-send).
    pub has_retry_loop: bool,
}

/// Compute the token-shape of `runs`. Pricing is consulted only to reuse real rates where the
/// model is priced; tokens are identical either way.
pub fn shape_stats(runs: &[RunRecord], pricing: &PricingTable) -> ShapeStats {
    let mut per_component_tokens: BTreeMap<Component, u64> = BTreeMap::new();
    let mut cache_read_tokens = 0u64;
    let mut cache_write_tokens = 0u64;
    let mut retry_available = true;
    let mut has_step = false;
    let mut has_retry_loop = false;
    for run in runs {
        let mut seen_hash: BTreeSet<u64> = BTreeSet::new();
        for step in &run.steps {
            has_step = true;
            match step.shape.request_hash {
                Some(h) => {
                    if !seen_hash.insert(h) {
                        has_retry_loop = true;
                    }
                }
                None => retry_available = false,
            }
            let allocations = pricing
                .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
                .map(|rates| allocate_step(&step.usage, rates, &step.shape))
                .unwrap_or_else(|| allocate_unpriced_step(&step.usage, &step.shape));
            for a in allocations {
                let component = per_component_tokens.entry(a.component).or_insert(0);
                *component = component.saturating_add(a.tokens);
                match a.class {
                    CacheClass::CacheRead => {
                        cache_read_tokens = cache_read_tokens.saturating_add(a.tokens)
                    }
                    CacheClass::CacheWrite5m | CacheClass::CacheWrite1h => {
                        cache_write_tokens = cache_write_tokens.saturating_add(a.tokens)
                    }
                    _ => {}
                }
            }
        }
    }
    // No steps at all -> nothing to assert about retries; treat as unavailable.
    if !has_step {
        retry_available = false;
    }
    ShapeStats {
        per_component_tokens,
        cache_read_tokens,
        cache_write_tokens,
        retry_available,
        has_retry_loop,
    }
}

impl ShapeStats {
    pub fn component_tokens(&self, c: Component) -> u64 {
        self.per_component_tokens.get(&c).copied().unwrap_or(0)
    }
    pub fn system_tokens(&self) -> u64 {
        self.component_tokens(Component::System)
    }
    pub fn tool_def_tokens(&self) -> u64 {
        self.component_tokens(Component::Tools)
    }

    /// Does cache-read make up at least `ratio_permille` parts-per-1000 of all cache traffic?
    /// Integer cross-multiply: `read*1000 >= ratio*(read+write)`. Vacuously true when there
    /// is no cache activity at all.
    pub fn meets_cache_read_ratio(&self, ratio_permille: u64) -> bool {
        let denom = self.cache_read_tokens as u128 + self.cache_write_tokens as u128;
        if denom == 0 {
            return true;
        }
        self.cache_read_tokens as u128 * 1000 >= ratio_permille as u128 * denom
    }
}

/// One per-component growth violation vs a baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrowthViolation {
    pub component: Component,
    pub baseline_tokens: u64,
    pub current_tokens: u64,
}

/// Components whose token count grew more than `pct`% vs `baseline`. Integer cross-multiply:
/// flag when `cur*100 > (100+pct)*base`. A component that appears anew (base 0, cur > 0) is a
/// violation regardless of `pct` (unbounded growth). Deterministic component order.
pub fn component_growth_violations(
    baseline: &ShapeStats,
    current: &ShapeStats,
    pct: u64,
) -> Vec<GrowthViolation> {
    let mut out = Vec::new();
    let comps: BTreeSet<Component> = baseline
        .per_component_tokens
        .keys()
        .chain(current.per_component_tokens.keys())
        .copied()
        .collect();
    for c in comps {
        let base = baseline.component_tokens(c) as u128;
        let cur = current.component_tokens(c) as u128;
        let violated = if base == 0 {
            cur > 0
        } else {
            cur * 100 > (100 + pct as u128) * base
        };
        if violated {
            out.push(GrowthViolation {
                component: c,
                baseline_tokens: base as u64,
                current_tokens: cur as u64,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_step;
    use crate::model::Provider;

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }
    fn fixture(rel: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("fixtures")
                .join(rel),
        )
        .unwrap()
    }

    fn bloated_run() -> Vec<RunRecord> {
        let mut steps = Vec::new();
        for i in 1..=3u32 {
            steps.push(
                ingest_step(
                    "r",
                    i,
                    Provider::Anthropic,
                    &fixture(&format!("bloated_system_prompt/step{i}.request.json")),
                    &fixture(&format!("bloated_system_prompt/step{i}.response.json")),
                )
                .unwrap(),
            );
        }
        vec![RunRecord {
            run_id: "r".into(),
            steps,
        }]
    }

    #[test]
    fn system_tokens_come_from_allocate_step_and_are_nonzero() {
        let runs = bloated_run();
        let s = shape_stats(&runs, &pricing());
        // The bloated-system-prompt fixture is dominated by an uncached system prompt.
        assert!(s.system_tokens() > 0, "system tokens should be attributed");
        let expected = runs[0]
            .steps
            .iter()
            .fold(0u64, |sum, step| sum.saturating_add(step.usage.total()));
        let attributed = s
            .per_component_tokens
            .values()
            .copied()
            .fold(0u64, u64::saturating_add);
        assert_eq!(
            attributed, expected,
            "every fixture token is attributed once"
        );
    }

    #[test]
    fn cache_read_ratio_is_integer_and_vacuous_without_cache() {
        // The non-streamed openai fixture has no cache traffic -> ratio vacuously satisfied.
        let run = vec![RunRecord {
            run_id: "o".into(),
            steps: vec![ingest_step(
                "o",
                1,
                Provider::Openai,
                &fixture("openai_nonstream/request.json"),
                &fixture("openai_nonstream/response.json"),
            )
            .unwrap()],
        }];
        let s = shape_stats(&run, &pricing());
        assert_eq!(s.cache_read_tokens + s.cache_write_tokens, 0);
        assert!(s.meets_cache_read_ratio(1000)); // even 100% required is vacuously OK
    }

    #[test]
    fn retry_detection_unavailable_when_request_hash_is_absent() {
        let mut runs = bloated_run();
        // Simulate the max_private profile: drop the request hashes.
        for step in &mut runs[0].steps {
            step.shape.request_hash = None;
        }
        let s = shape_stats(&runs, &pricing());
        assert!(
            !s.retry_available,
            "no request_hash -> retry detection must be unavailable (fail-closed at the gate)"
        );
    }

    #[test]
    fn component_growth_flags_new_and_grown_components() {
        let mut base = ShapeStats::default();
        base.per_component_tokens.insert(Component::System, 100);
        let mut cur = ShapeStats::default();
        cur.per_component_tokens.insert(Component::System, 150); // +50%
        cur.per_component_tokens.insert(Component::Tools, 10); // brand new

        // 60% allowance: System (+50%) is fine, but the new Tools component is unbounded growth.
        let v = component_growth_violations(&base, &cur, 60);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].component, Component::Tools);

        // 40% allowance: System (+50%) now also trips.
        let v = component_growth_violations(&base, &cur, 40);
        assert!(v.iter().any(|x| x.component == Component::System));
    }

    #[test]
    fn aggregate_component_counts_saturate() {
        let mut runs = bloated_run();
        let template = runs[0].steps[0].clone();
        runs[0].steps = vec![template.clone(), template];
        for step in &mut runs[0].steps {
            step.usage = crate::model::UsageTokens {
                fresh_input: u64::MAX,
                ..Default::default()
            };
            step.shape.weights = vec![crate::model::ComponentWeight {
                component: Component::System,
                bytes: 1,
            }];
        }
        let stats = shape_stats(&runs, &pricing());
        assert_eq!(stats.system_tokens(), u64::MAX);
    }
}
