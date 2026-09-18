//! Prompt/config lineage: a user-named, ordered sequence of prompt-component VERSIONS,
//! each an immutable content-addressed fingerprint (`system_hash`). Because the fingerprint is
//! content-addressed, every distinct hash IS a distinct, immutable version — no bookkeeping, no
//! payloads. Given the versions, Tare projects cost-per-run for each so you can answer "did my
//! rewrite (v3) hold quality at lower cost than v2?".
//!
//! PURE re-projection of already-captured rows: it runs the tested `rollup(Template)` engine once and
//! indexes its per-`template#<hash>` rows, so it adds no capture surface and costs nothing. All costs
//! ESTIMATED, never billed.

use crate::model::RunRecord;
use crate::pricing::PricingTable;
use crate::rollup::{rollup, RollupDim};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One labelled version in a lineage: a human name (`v3`) bound to an immutable component fingerprint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineageVersion {
    pub label: String,
    /// The prompt-component fingerprint (`step.shape.system_hash`).
    pub hash: u64,
}

/// A named lineage: the ordered versions of one prompt/component to compare over time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lineage {
    pub name: String,
    #[serde(default)]
    pub versions: Vec<LineageVersion>,
}

/// One version projected onto its outcomes: how many runs carried this fingerprint and what it cost.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageVersionRow {
    pub label: String,
    pub hash: u64,
    /// Distinct runs that carried a step with this fingerprint.
    pub runs: u32,
    pub steps: u32,
    pub tokens: u64,
    /// Estimated spend attributed to this fingerprint's steps (micro-USD).
    pub cost_micros: i64,
    /// The comparison metric: `cost_micros / runs` (0 when this version has no runs yet).
    pub micros_per_run: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageReport {
    pub name: String,
    /// Rows in the lineage's DECLARED version order (so v1→v2→v3 reads left-to-right); a version with
    /// no captured runs yet is present with zeroes rather than dropped (honest "not seen yet").
    pub rows: Vec<LineageVersionRow>,
    pub pricing_version: String,
    /// Every figure here is ESTIMATED.
    pub estimated: bool,
}

/// Project a lineage's versions onto cost-per-run. Deterministic; pure. Reuses the
/// `rollup(Template)` engine — each `template#<hash>` bucket already carries runs/steps/tokens/micros.
pub fn lineage_report(
    runs: &[RunRecord],
    pricing: &PricingTable,
    lineage: &Lineage,
) -> LineageReport {
    let roll = rollup(runs, pricing, RollupDim::Template);
    // Index the template buckets by their numeric fingerprint (label is `template#<hash>`).
    let by_hash: HashMap<u64, &crate::rollup::RollupRow> = roll
        .rows
        .iter()
        .filter_map(|r| {
            r.label
                .strip_prefix("template#")
                .and_then(|h| h.parse::<u64>().ok())
                .map(|h| (h, r))
        })
        .collect();
    let rows = lineage
        .versions
        .iter()
        .map(|v| match by_hash.get(&v.hash) {
            Some(r) => LineageVersionRow {
                label: v.label.clone(),
                hash: v.hash,
                runs: r.runs,
                steps: r.steps,
                tokens: r.tokens,
                cost_micros: r.micros,
                micros_per_run: if r.runs > 0 {
                    r.micros / r.runs as i64
                } else {
                    0
                },
            },
            None => LineageVersionRow {
                label: v.label.clone(),
                hash: v.hash,
                runs: 0,
                steps: 0,
                tokens: 0,
                cost_micros: 0,
                micros_per_run: 0,
            },
        })
        .collect();
    LineageReport {
        name: lineage.name.clone(),
        rows,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};

    fn step(system_hash: Option<u64>, input: u64) -> StepRecord {
        StepRecord {
            run_id: "r".into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: UsageTokens {
                fresh_input: input,
                ..Default::default()
            },
            shape: RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: CacheTtl::FiveMin,
                has_cache_control: false,
                cached_component: None,
                system_hash,
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

    fn run(id: &str, steps: Vec<StepRecord>) -> RunRecord {
        RunRecord {
            run_id: id.into(),
            steps,
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
    fn projects_cost_per_run_per_version_in_declared_order() {
        // v1 (hash 1): two runs, 1M input each → $3/run. v2 (hash 2): one run, 1M → $3 but the
        // rewrite is cheaper per run because... here we make v2 half the tokens to show the win.
        let runs = vec![
            run("a", vec![step(Some(1), 1_000_000)]),
            run("b", vec![step(Some(1), 1_000_000)]),
            run("c", vec![step(Some(2), 500_000)]),
        ];
        let lineage = Lineage {
            name: "checkout".into(),
            versions: vec![
                LineageVersion {
                    label: "v1".into(),
                    hash: 1,
                },
                LineageVersion {
                    label: "v2".into(),
                    hash: 2,
                },
                LineageVersion {
                    label: "v3".into(),
                    hash: 3,
                }, // not captured yet
            ],
        };
        let rep = lineage_report(&runs, &pricing(), &lineage);
        assert_eq!(rep.rows.len(), 3);
        // Declared order preserved.
        assert_eq!(
            rep.rows
                .iter()
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>(),
            vec!["v1", "v2", "v3"]
        );
        // v1: 2 runs, $6 total → $3/run.
        let v1 = &rep.rows[0];
        assert_eq!(v1.runs, 2);
        assert_eq!(v1.cost_micros, 6_000_000);
        assert_eq!(v1.micros_per_run, 3_000_000);
        // v2: the rewrite — 1 run, 0.5M @ $3 = $1.5/run, cheaper per run.
        let v2 = &rep.rows[1];
        assert_eq!(v2.runs, 1);
        assert_eq!(v2.micros_per_run, 1_500_000);
        assert!(
            v2.micros_per_run < v1.micros_per_run,
            "v2 rewrite is cheaper per run"
        );
        // v3: declared but unseen → present with zeroes, not dropped (honest).
        let v3 = &rep.rows[2];
        assert_eq!(v3.runs, 0);
        assert_eq!(v3.cost_micros, 0);
        assert_eq!(v3.micros_per_run, 0);
        assert!(rep.estimated);
    }
}
