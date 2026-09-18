//! Spend rollup: aggregate cost by an adapter correlation label (step / component / parent).
//! Pure function of stored steps + pricing; integer micro-USD; deterministic (BTreeMap order).
//! Answers "which sub-agent / tool / parent burned the money?" over labels already captured on
//! each step. Unknown-model steps contribute their tokens but $0 — the same honest
//! treatment as trends and attribution — and are surfaced separately through `Report.unpriced`.

use crate::account::cost_usage;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Which correlation label to group by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollupDim {
    Step,
    Component,
    Parent,
    /// Agent-native aliases: `Tool` reads the component label, `Agent` reads the parent label —
    /// the same storage, named in the agent users' vocabulary ("which tool / which subagent?").
    Tool,
    Agent,
    /// Reasoning effort level (low/medium/high/xhigh/max).
    Effort,
    /// Cost dimensions (not correlation labels): the step's model, provider, and owning agent
    /// session/conversation. Power the Live "top spenders" pivot and the spend-by-model treemap.
    Model,
    Provider,
    Session,
    /// The MCP server that originated the request (OTel-captured only; proxy steps lack it and
    /// bucket as `unlabeled`). Answers "which MCP server is expensive?"
    McpServer,
    /// Git-native cost: the commit SHA / author the working tree was on. Answers
    /// "which commit / whose work cost the most?" — git-blame-for-cost.
    Commit,
    Author,
    /// Prompt-template fingerprint: the stable-prefix hash (`system_hash`) that groups
    /// every request sharing a system+tools prefix into one "template" — the controllable unit behind
    /// spend, the reframe's headline driver. Proxy-captured steps carry it; backfilled steps don't
    /// (`system_hash = None` → `unlabeled`).
    Template,
}

impl RollupDim {
    pub fn as_str(self) -> &'static str {
        match self {
            RollupDim::Step => "step",
            RollupDim::Component => "component",
            RollupDim::Parent => "parent",
            RollupDim::Tool => "tool",
            RollupDim::Agent => "agent",
            RollupDim::Effort => "effort",
            RollupDim::Model => "model",
            RollupDim::Provider => "provider",
            RollupDim::Session => "session",
            RollupDim::McpServer => "mcp_server",
            RollupDim::Commit => "commit",
            RollupDim::Author => "author",
            RollupDim::Template => "template",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "step" => Some(RollupDim::Step),
            "component" => Some(RollupDim::Component),
            "parent" => Some(RollupDim::Parent),
            "tool" => Some(RollupDim::Tool),
            "agent" => Some(RollupDim::Agent),
            "effort" => Some(RollupDim::Effort),
            "model" => Some(RollupDim::Model),
            "provider" => Some(RollupDim::Provider),
            "session" => Some(RollupDim::Session),
            "mcp_server" => Some(RollupDim::McpServer),
            "commit" => Some(RollupDim::Commit),
            "author" => Some(RollupDim::Author),
            "template" => Some(RollupDim::Template),
            _ => None,
        }
    }
}

/// The label this dim reads off a step (None -> the `unlabeled` bucket). Most dims read a
/// correlation label off the shape; the cost dims read the step's own model/provider/session; the
/// template dim derives an owned label from the numeric prompt-template fingerprint. Returns an
/// owned String so a synthesized label (template#<hash>) composes with the borrowed ones.
fn label_of(step: &crate::model::StepRecord, dim: RollupDim) -> Option<String> {
    match dim {
        RollupDim::Step => step.shape.step_label.clone(),
        RollupDim::Component | RollupDim::Tool => step.shape.component_label.clone(),
        RollupDim::Parent | RollupDim::Agent => step.shape.parent_label.clone(),
        RollupDim::Effort => step.shape.effort.clone(),
        // An empty model (fully degraded capture) buckets as `unlabeled`, not "".
        RollupDim::Model => (!step.model.is_empty()).then(|| step.model.clone()),
        RollupDim::Provider => Some(step.provider.as_str().to_string()),
        RollupDim::Session => step.shape.session.clone(),
        RollupDim::McpServer => step.shape.mcp_server.clone(),
        RollupDim::Commit => step.shape.commit.clone(),
        RollupDim::Author => step.shape.author.clone(),
        // Prompt-template fingerprint: group by the stable-prefix hash.
        RollupDim::Template => step.shape.system_hash.map(|h| format!("template#{h}")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupRow {
    pub label: String,
    pub runs: u32,
    pub steps: u32,
    pub tokens: u64,
    pub micros: i64,
    /// Mean micro-USD per step in this bucket (integer; 0 when no steps).
    pub micros_per_call: i64,
    /// The run ids in this bucket (sorted), for the group-by drill-down to member runs.
    /// Omitted from JSON when empty so existing goldens stay byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupReport {
    pub dimension: String,
    pub rows: Vec<RollupRow>,
    pub total_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

#[derive(Default)]
struct Bucket {
    runs: BTreeSet<String>,
    steps: u32,
    tokens: u64,
    micros: i64,
}

/// Aggregate spend by `dim`'s label across `runs`. Rows are sorted by micros desc, then label
/// asc (deterministic). Steps with no label fall into an `unlabeled` bucket.
pub fn rollup(runs: &[RunRecord], pricing: &PricingTable, dim: RollupDim) -> RollupReport {
    rollup_filtered(runs, pricing, dim, None)
}

/// As [`rollup`], but first restrict to the STEPS matching a parent `(filter_dim, label)` — the
/// enabler for the progressive drill (e.g. bucket by `session` WHERE `template` = X: "how did
/// template X's spend split across sessions?"). Only the matching steps' cost is attributed, so the
/// filtered rows reconcile to the filtered total. `None` → identical to `rollup` (byte-for-byte).
pub fn rollup_filtered(
    runs: &[RunRecord],
    pricing: &PricingTable,
    dim: RollupDim,
    filter: Option<(RollupDim, &str)>,
) -> RollupReport {
    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = 0i64;
    for run in runs {
        for step in &run.steps {
            // Parent-dimension filter: skip steps whose label under the filter dim doesn't match.
            if let Some((fdim, flabel)) = filter {
                if label_of(step, fdim).as_deref() != Some(flabel) {
                    continue;
                }
            }
            let key = label_of(step, dim).unwrap_or_else(|| "unlabeled".to_string());
            let micros = pricing
                .lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
                .map(|r| cost_usage(&step.usage, r, &step.shape).total.micros())
                .unwrap_or(0);
            let b = buckets.entry(key).or_default();
            b.runs.insert(run.run_id.clone());
            b.steps = b.steps.saturating_add(1);
            b.tokens = b.tokens.saturating_add(step.usage.total());
            b.micros = b.micros.saturating_add(micros);
            total = total.saturating_add(micros);
        }
    }
    let mut rows: Vec<RollupRow> = buckets
        .into_iter()
        .map(|(label, b)| RollupRow {
            label,
            runs: u32::try_from(b.runs.len()).unwrap_or(u32::MAX),
            steps: b.steps,
            tokens: b.tokens,
            micros: b.micros,
            micros_per_call: if b.steps == 0 {
                0
            } else {
                b.micros / b.steps as i64
            },
            members: b.runs.iter().cloned().collect(), // BTreeSet -> sorted run ids (drill-down)
        })
        .collect();
    rows.sort_by(|a, b| b.micros.cmp(&a.micros).then(a.label.cmp(&b.label)));
    RollupReport {
        dimension: dim.as_str().to_string(),
        rows,
        total_micros: total,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, StepMeta};

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }

    fn step_with_label(ord: u32, label: Option<&str>) -> crate::model::StepRecord {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let meta = StepMeta {
            workload_key: None,
            step_label: label.map(String::from),
            ..Default::default()
        };
        crate::ingest_step_ct(
            "r",
            ord,
            Provider::Openai,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            &meta,
            None,
            0,
        )
        .unwrap()
    }

    #[test]
    fn rolls_up_by_step_label_with_unlabeled_bucket() {
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                step_with_label(1, Some("plan")),
                step_with_label(2, Some("plan")),
                step_with_label(3, None),
            ],
        }];
        let rep = rollup(&runs, &pricing(), RollupDim::Step);
        assert_eq!(rep.rows.len(), 2);
        // "plan" has 2 steps and the larger total -> ranked first.
        assert_eq!(rep.rows[0].label, "plan");
        assert_eq!(rep.rows[0].steps, 2);
        assert_eq!(rep.rows[0].runs, 1);
        assert_eq!(rep.rows[0].micros_per_call, rep.rows[0].micros / 2);
        assert_eq!(rep.rows[1].label, "unlabeled");
        // Σ buckets == total.
        let sum: i64 = rep.rows.iter().map(|r| r.micros).sum();
        assert_eq!(sum, rep.total_micros);
    }

    fn step_with_meta(ord: u32, meta: &StepMeta) -> crate::model::StepRecord {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        crate::ingest_step_ct(
            "r",
            ord,
            Provider::Openai,
            req,
            resp,
            None,
            &crate::PrivacyPolicy::default(),
            meta,
            None,
            0,
        )
        .unwrap()
    }

    #[test]
    fn rollup_filtered_restricts_to_the_parent_dimension() {
        // bucket by component, filtered to only the steps whose step-label is "plan".
        let mk = |ord, step, comp| {
            step_with_meta(
                ord,
                &StepMeta {
                    workload_key: None,
                    step_label: Some(String::from(step)),
                    component_label: Some(String::from(comp)),
                    ..Default::default()
                },
            )
        };
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![
                mk(1, "plan", "search"),
                mk(2, "plan", "edit"),
                mk(3, "exec", "search"),
            ],
        }];
        // Unfiltered: "search" has 2 steps (plan + exec).
        let all = rollup(&runs, &pricing(), RollupDim::Component);
        assert_eq!(
            all.rows.iter().find(|r| r.label == "search").unwrap().steps,
            2
        );
        // Filtered to step=plan: only s1 (search) + s2 (edit) count → search has 1 step, and the
        // filtered total is just those two steps' cost (reconciles to Σ rows).
        let filtered = rollup_filtered(
            &runs,
            &pricing(),
            RollupDim::Component,
            Some((RollupDim::Step, "plan")),
        );
        assert_eq!(
            filtered
                .rows
                .iter()
                .find(|r| r.label == "search")
                .unwrap()
                .steps,
            1
        );
        assert_eq!(filtered.rows.iter().map(|r| r.steps).sum::<u32>(), 2);
        assert_eq!(
            filtered.rows.iter().map(|r| r.micros).sum::<i64>(),
            filtered.total_micros
        );
        assert!(
            filtered.total_micros < all.total_micros,
            "the exec step's cost is excluded"
        );
        // None filter is byte-identical to rollup().
        assert_eq!(
            rollup_filtered(&runs, &pricing(), RollupDim::Component, None),
            all
        );
    }

    #[test]
    fn rolls_up_by_cost_dims_model_provider_session() {
        // The new dims parse and round-trip through as_str.
        for d in ["model", "provider", "session", "mcp_server"] {
            assert_eq!(RollupDim::parse(d).unwrap().as_str(), d);
        }
        let mut step = step_with_meta(1, &StepMeta::default());
        step.shape.session = Some("conv-1".into()); // session lives on the shape, set at ingest
        let model = step.model.clone();
        assert!(!model.is_empty(), "fixture carries a model");
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step],
        }];

        // Provider: the step's wire provider, not a correlation label.
        assert_eq!(
            rollup(&runs, &pricing(), RollupDim::Provider).rows[0].label,
            "openai"
        );
        // Model: the step's own model string.
        assert_eq!(
            rollup(&runs, &pricing(), RollupDim::Model).rows[0].label,
            model
        );
        // Session: the owning conversation/session id from the shape.
        assert_eq!(
            rollup(&runs, &pricing(), RollupDim::Session).rows[0].label,
            "conv-1"
        );
    }

    #[test]
    fn rolls_up_by_git_commit_and_author() {
        // git-native cost dims parse + read the shape's git labels.
        for d in ["commit", "author"] {
            assert_eq!(RollupDim::parse(d).unwrap().as_str(), d);
        }
        let mk = |run: &str, commit: &str, author: &str| {
            let mut s = step_with_meta(1, &StepMeta::default());
            s.shape.commit = Some(commit.into());
            s.shape.author = Some(author.into());
            RunRecord {
                run_id: run.into(),
                steps: vec![s],
            }
        };
        // Two runs on commit "abc" (one each by two authors), one on "def".
        let runs = vec![
            mk("r1", "abc1234", "Alice"),
            mk("r2", "abc1234", "Bob"),
            mk("r3", "def5678", "Alice"),
        ];
        let by_commit = rollup(&runs, &pricing(), RollupDim::Commit);
        let abc = by_commit
            .rows
            .iter()
            .find(|r| r.label == "abc1234")
            .unwrap();
        assert_eq!(abc.runs, 2, "two runs on abc1234");
        let by_author = rollup(&runs, &pricing(), RollupDim::Author);
        let alice = by_author.rows.iter().find(|r| r.label == "Alice").unwrap();
        assert_eq!(alice.runs, 2, "Alice authored two runs (abc + def)");
    }

    #[test]
    fn rows_carry_sorted_member_run_ids_for_drilldown() {
        // each bucket exposes its distinct run ids (sorted) so the UI can drill in.
        let mk = |run: &str, ord: u32| {
            let s = step_with_meta(ord, &StepMeta::default());
            RunRecord {
                run_id: run.into(),
                steps: vec![s],
            }
        };
        // Two runs on the same model bucket -> both listed, sorted, deduped.
        let runs = vec![mk("run-b", 1), mk("run-a", 2)];
        let row = &rollup(&runs, &pricing(), RollupDim::Model).rows[0];
        assert_eq!(row.runs, 2);
        assert_eq!(row.members, vec!["run-a", "run-b"]);
    }

    #[test]
    fn model_dim_empty_model_buckets_as_unlabeled() {
        // A fully degraded step (no model) must not produce an empty-string bucket.
        let mut step = step_with_meta(1, &StepMeta::default());
        step.model = String::new();
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![step],
        }];
        assert_eq!(
            rollup(&runs, &pricing(), RollupDim::Model).rows[0].label,
            "unlabeled"
        );
    }

    #[test]
    fn rolls_up_by_mcp_server_with_unlabeled_bucket() {
        // A step whose shape carries an MCP-server label rolls up under that label…
        let mut labeled = step_with_meta(1, &StepMeta::default());
        labeled.shape.mcp_server = Some("github-mcp".into());
        // …and one without (e.g. proxy-captured) buckets as unlabeled, in the same report.
        let bare = step_with_meta(2, &StepMeta::default());
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![labeled, bare],
        }];
        let rep = rollup(&runs, &pricing(), RollupDim::McpServer);
        let labels: Vec<&str> = rep.rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"github-mcp"), "got {labels:?}");
        assert!(labels.contains(&"unlabeled"), "got {labels:?}");
    }

    #[test]
    fn rolls_up_by_prompt_template_fingerprint() {
        // Two steps sharing a system_hash group into one template#<hash> row; a distinct hash is its
        // own row; a step without a fingerprint (backfilled) buckets as unlabeled.
        let mut a1 = step_with_meta(1, &StepMeta::default());
        a1.shape.system_hash = Some(1);
        let mut a2 = step_with_meta(2, &StepMeta::default());
        a2.shape.system_hash = Some(1);
        let mut b = step_with_meta(3, &StepMeta::default());
        b.shape.system_hash = Some(2);
        let mut bare = step_with_meta(4, &StepMeta::default());
        bare.shape.system_hash = None; // no fingerprint -> unlabeled
        let runs = vec![RunRecord {
            run_id: "r".into(),
            steps: vec![a1, a2, b, bare],
        }];
        let rep = rollup(&runs, &pricing(), RollupDim::Template);
        let labels: Vec<&str> = rep.rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"template#1"), "got {labels:?}");
        assert!(labels.contains(&"template#2"), "got {labels:?}");
        assert!(labels.contains(&"unlabeled"), "got {labels:?}");
        let t1 = rep.rows.iter().find(|r| r.label == "template#1").unwrap();
        assert_eq!(t1.steps, 2, "both same-hash steps grouped");
        // Σ buckets == total; dim round-trips through parse/as_str.
        let sum: i64 = rep.rows.iter().map(|r| r.micros).sum();
        assert_eq!(sum, rep.total_micros);
        assert_eq!(RollupDim::parse("template").unwrap().as_str(), "template");
    }

    #[test]
    fn empty_runs_yield_no_rows() {
        let rep = rollup(&[], &pricing(), RollupDim::Component);
        assert!(rep.rows.is_empty());
        assert_eq!(rep.total_micros, 0);
    }
}
