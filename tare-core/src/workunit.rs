//! Cost-per-unit-of-work + retroactive relabel: dev-meaningful DENOMINATORS. A user
//! declares "units of work" (a task, a PR, a feature) as match rules, and Tare retroactively buckets
//! already-captured runs into them — answering "what did this buy me?" without revenue/tagging infra.
//! FinOps unit-cost, done locally on counts.
//!
//! PURE re-projection of stored rows: a run is bucketed by matching its steps' session/commit against
//! the declared rules (no re-capture, no payloads). Each run lands in AT MOST ONE unit — the first it
//! matches, in declared order — so per-unit spend never double-counts and the total reconciles; runs
//! matching no unit are surfaced in an explicit `unbucketed` row (coverage honesty). All ESTIMATED.

use crate::attribute::total_micros;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};

/// How a unit of work claims runs. Criteria AND together; within a list, ANY value hits.
/// An all-empty match claims nothing (a rule must say what it captures).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitMatch {
    /// A run matches if any step's `shape.session` is in this set.
    #[serde(default)]
    pub sessions: Vec<String>,
    /// A run matches if any step's `shape.commit` is in this set.
    #[serde(default)]
    pub commits: Vec<String>,
    /// A run matches if its `run_id` starts with this prefix.
    #[serde(default)]
    pub run_prefix: Option<String>,
}

impl UnitMatch {
    fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.commits.is_empty() && self.run_prefix.is_none()
    }

    /// Does `run` satisfy every SPECIFIED criterion? An empty match claims nothing.
    fn matches(&self, run: &RunRecord) -> bool {
        if self.is_empty() {
            return false;
        }
        if let Some(p) = &self.run_prefix {
            if !run.run_id.starts_with(p) {
                return false;
            }
        }
        if !self.sessions.is_empty()
            && !run.steps.iter().any(|s| {
                s.shape
                    .session
                    .as_deref()
                    .is_some_and(|v| self.sessions.iter().any(|x| x == v))
            })
        {
            return false;
        }
        if !self.commits.is_empty()
            && !run.steps.iter().any(|s| {
                s.shape
                    .commit
                    .as_deref()
                    .is_some_and(|v| self.commits.iter().any(|x| x == v))
            })
        {
            return false;
        }
        true
    }
}

/// A declared unit of work: a human name + the rule that claims its runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkUnit {
    pub name: String,
    #[serde(default, rename = "match")]
    pub match_: UnitMatch,
}

/// One unit projected onto its cost denominator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRow {
    pub name: String,
    pub runs: u32,
    pub steps: u32,
    pub tokens: u64,
    pub cost_micros: i64,
    /// The denominator: `cost_micros / runs` (0 when the unit claimed no runs).
    pub micros_per_run: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitReport {
    /// Declared units in order, then a trailing `unbucketed` row for runs no rule claimed.
    pub rows: Vec<UnitRow>,
    /// Runs claimed by no unit (the `unbucketed` row's run count) — surfaced, never hidden.
    pub unbucketed_runs: u32,
    pub total_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Bucket runs into units of work and price each. Deterministic; pure. Each run lands in
/// the first unit it matches (declared order); the remainder go to an `unbucketed` row.
pub fn unit_report(runs: &[RunRecord], pricing: &PricingTable, units: &[WorkUnit]) -> UnitReport {
    let mut rows: Vec<UnitRow> = units
        .iter()
        .map(|u| UnitRow {
            name: u.name.clone(),
            runs: 0,
            steps: 0,
            tokens: 0,
            cost_micros: 0,
            micros_per_run: 0,
        })
        .collect();
    let mut unbucketed = UnitRow {
        name: "unbucketed".to_string(),
        runs: 0,
        steps: 0,
        tokens: 0,
        cost_micros: 0,
        micros_per_run: 0,
    };
    let mut total = 0i64;
    for run in runs {
        let cost = total_micros(std::slice::from_ref(run), pricing);
        let steps = u32::try_from(run.steps.len()).unwrap_or(u32::MAX);
        let tokens: u64 = run
            .steps
            .iter()
            .fold(0u64, |a, s| a.saturating_add(s.usage.total()));
        total = total.saturating_add(cost);
        // First matching unit in declared order; else the unbucketed catch-all.
        let dst = units
            .iter()
            .position(|u| u.match_.matches(run))
            .map(|i| &mut rows[i])
            .unwrap_or(&mut unbucketed);
        dst.runs = dst.runs.saturating_add(1);
        dst.steps = dst.steps.saturating_add(steps);
        dst.tokens = dst.tokens.saturating_add(tokens);
        dst.cost_micros = dst.cost_micros.saturating_add(cost);
    }
    for r in rows.iter_mut().chain(std::iter::once(&mut unbucketed)) {
        r.micros_per_run = if r.runs > 0 {
            r.cost_micros / r.runs as i64
        } else {
            0
        };
    }
    let unbucketed_runs = unbucketed.runs;
    rows.push(unbucketed);
    UnitReport {
        rows,
        unbucketed_runs,
        total_micros: total,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CacheTtl, Provider, RequestShape, StepRecord, UsageTokens};

    fn step(input: u64, session: Option<&str>, commit: Option<&str>) -> StepRecord {
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
                system_hash: None,
                weights: vec![],
                request_hash: Some(0),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: session.map(String::from),
                workload_key: None,
                effort: None,
                mcp_server: None,
                vendor: None,
                commit: commit.map(String::from),
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
    fn buckets_runs_into_units_and_surfaces_the_rest() {
        let runs = vec![
            run("a", vec![step(1_000_000, Some("task-1"), None)]),
            run("b", vec![step(1_000_000, Some("task-1"), None)]),
            run("c", vec![step(1_000_000, None, Some("deadbeef"))]),
            run("d", vec![step(1_000_000, Some("other"), None)]), // matches no unit
        ];
        let units = vec![
            WorkUnit {
                name: "PR #1".into(),
                match_: UnitMatch {
                    sessions: vec!["task-1".into()],
                    ..Default::default()
                },
            },
            WorkUnit {
                name: "hotfix".into(),
                match_: UnitMatch {
                    commits: vec!["deadbeef".into()],
                    ..Default::default()
                },
            },
        ];
        let rep = unit_report(&runs, &pricing(), &units);
        // Rows: PR #1, hotfix, unbucketed.
        assert_eq!(rep.rows.len(), 3);
        let pr = &rep.rows[0];
        assert_eq!(pr.name, "PR #1");
        assert_eq!(pr.runs, 2);
        assert_eq!(pr.cost_micros, 6_000_000); // 2 × 1M @ $3
        assert_eq!(pr.micros_per_run, 3_000_000);
        let hotfix = &rep.rows[1];
        assert_eq!(hotfix.runs, 1);
        assert_eq!(hotfix.cost_micros, 3_000_000);
        // Run d matched nothing → unbucketed, surfaced honestly.
        let unb = &rep.rows[2];
        assert_eq!(unb.name, "unbucketed");
        assert_eq!(unb.runs, 1);
        assert_eq!(rep.unbucketed_runs, 1);
        // Total reconciles across all four runs (no double-count).
        assert_eq!(rep.total_micros, 12_000_000);
        assert_eq!(
            rep.rows.iter().map(|r| r.cost_micros).sum::<i64>(),
            rep.total_micros
        );
        assert!(rep.estimated);
    }

    #[test]
    fn a_run_lands_in_only_the_first_matching_unit() {
        // Run matches both units; declared order wins → no double-count.
        let runs = vec![run("a", vec![step(1_000_000, Some("s"), Some("c"))])];
        let units = vec![
            WorkUnit {
                name: "first".into(),
                match_: UnitMatch {
                    sessions: vec!["s".into()],
                    ..Default::default()
                },
            },
            WorkUnit {
                name: "second".into(),
                match_: UnitMatch {
                    commits: vec!["c".into()],
                    ..Default::default()
                },
            },
        ];
        let rep = unit_report(&runs, &pricing(), &units);
        assert_eq!(rep.rows[0].runs, 1, "first declared unit claims it");
        assert_eq!(rep.rows[1].runs, 0, "second does not double-count");
        assert_eq!(rep.total_micros, 3_000_000);
    }

    #[test]
    fn an_empty_match_claims_nothing() {
        let runs = vec![run("a", vec![step(1_000_000, Some("s"), None)])];
        let units = vec![WorkUnit {
            name: "catch-all?".into(),
            match_: UnitMatch::default(),
        }];
        let rep = unit_report(&runs, &pricing(), &units);
        assert_eq!(
            rep.rows[0].runs, 0,
            "an empty rule must not silently swallow everything"
        );
        assert_eq!(rep.unbucketed_runs, 1);
    }
}
