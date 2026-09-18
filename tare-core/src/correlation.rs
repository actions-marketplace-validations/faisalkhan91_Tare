//! Config↔outcome correlation (the "Explain" panel): re-project stored runs onto
//! config-knob + outcome axes for a many-run parallel-coordinates plot — "which configuration
//! choices ride with high cost?". PURE and ZERO new capture: a projection of already-stored runs +
//! pricing, so it costs nothing and adds no capture surface. Feeds the framework-free `parcoords`
//! primitive on-screen.

use crate::attribute::total_micros_dated;
use crate::model::{CacheTtl, RunRecord};
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One run projected onto the correlation axes: its config knobs + its outcomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrelationRow {
    pub run_id: String,
    /// The run's model (first step; runs are typically single-model).
    pub model: String,
    /// Whether ANY step declared `cache_control` — the caching knob.
    pub cache_control: bool,
    /// Reasoning-effort tier if any step set one (first seen); `None` when unset.
    pub effort: Option<String>,
    /// Cache window requested: `"1h"` if any step asked for the 1h TTL, else `"5m"`.
    pub ttl: String,
    /// Estimated cost of this run (micro-USD) — the primary outcome.
    pub cost_micros: i64,
    /// Total tokens across the run.
    pub tokens: u64,
    /// Summed observed local latency (ms); `0` when unmeasured / suppressed.
    pub duration_ms: u64,
    pub steps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrelationReport {
    pub rows: Vec<CorrelationRow>,
    pub pricing_version: String,
    /// Every cost figure here is ESTIMATED, never billed.
    pub estimated: bool,
}

/// Project each run onto its config knobs + outcomes. Deterministic (preserves input order); pure.
pub fn correlate_runs(runs: &[RunRecord], pricing: &PricingTable) -> CorrelationReport {
    correlate_runs_dated(runs, pricing, &BTreeMap::new())
}

/// As [`correlate_runs`], but pricing each run as-of its capture day from `day_by_run`,
/// so the panel's per-run cost reconciles byte-for-byte with the report/receipt. Empty map →
/// identical to the undated path.
pub fn correlate_runs_dated(
    runs: &[RunRecord],
    pricing: &PricingTable,
    day_by_run: &BTreeMap<String, String>,
) -> CorrelationReport {
    let rows = runs
        .iter()
        .map(|run| {
            let mut cache_control = false;
            let mut effort: Option<String> = None;
            let mut one_hour = false;
            let mut tokens = 0u64;
            let mut duration_ms = 0u64;
            for step in &run.steps {
                cache_control |= step.shape.has_cache_control;
                if effort.is_none() {
                    effort = step.shape.effort.clone();
                }
                if matches!(step.shape.ttl, CacheTtl::OneHour) {
                    one_hour = true;
                }
                tokens = tokens
                    .saturating_add(step.usage.total())
                    .saturating_add(step.usage.audio_input)
                    .saturating_add(step.usage.audio_output);
                duration_ms = duration_ms.saturating_add(step.duration_ms);
            }
            CorrelationRow {
                run_id: run.run_id.clone(),
                model: run
                    .steps
                    .first()
                    .map(|s| s.model.clone())
                    .unwrap_or_default(),
                cache_control,
                effort,
                ttl: if one_hour { "1h".into() } else { "5m".into() },
                // Reprice this one run through the shared attribution path (honors overrides/overlay
                // + per-run AsOf), so cost matches the report byte-for-byte.
                cost_micros: total_micros_dated(std::slice::from_ref(run), pricing, day_by_run),
                tokens,
                duration_ms,
                steps: u32::try_from(run.steps.len()).unwrap_or(u32::MAX),
            }
        })
        .collect();
    CorrelationReport {
        rows,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, RequestShape, StepRecord, UsageTokens};

    fn step(model: &str, input: u64, cc: bool, effort: Option<&str>, ttl: CacheTtl) -> StepRecord {
        StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: model.into(),
            usage: UsageTokens {
                fresh_input: input,
                ..Default::default()
            },
            shape: RequestShape {
                model: model.into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl,
                has_cache_control: cc,
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
                effort: effort.map(|e| e.to_string()),
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

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(
            r#"
version = "v"
effective_date = "2026-06-01"
[[model]]
provider = "anthropic"
model_id = "m"
input_micro_per_mtok = 3000000
output_micro_per_mtok = 0
cache_read_micro_per_mtok = 0
cache_write_5m_micro_per_mtok = 0
cache_write_1h_micro_per_mtok = 0
"#,
        )
        .unwrap()
    }

    #[test]
    fn correlate_projects_config_knobs_and_outcomes() {
        let p = pricing();
        let runs = vec![
            RunRecord {
                run_id: "a".into(),
                steps: vec![step("m", 1_000_000, true, Some("high"), CacheTtl::OneHour)],
            },
            RunRecord {
                run_id: "b".into(),
                steps: vec![step("m", 500_000, false, None, CacheTtl::FiveMin)],
            },
        ];
        let rep = correlate_runs(&runs, &p);
        assert_eq!(rep.rows.len(), 2);
        // Row a: cache-control on, 1h TTL, high effort, 1M tokens @ $3 = 3M micro-USD.
        let a = &rep.rows[0];
        assert_eq!(a.model, "m");
        assert!(a.cache_control);
        assert_eq!(a.effort.as_deref(), Some("high"));
        assert_eq!(a.ttl, "1h");
        assert_eq!(a.tokens, 1_000_000);
        assert_eq!(a.cost_micros, 3_000_000);
        // Row b: no cache-control, 5m TTL, no effort, 0.5M @ $3 = 1.5M.
        let b = &rep.rows[1];
        assert!(!b.cache_control);
        assert_eq!(b.ttl, "5m");
        assert!(b.effort.is_none());
        assert_eq!(b.cost_micros, 1_500_000);
    }
}
