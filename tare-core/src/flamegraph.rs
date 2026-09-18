//! Pure flamegraph model: run -> step -> component -> cache-class leaf.
//! Width = tokens or dollars; color = cache class. Deterministically ordered.

use crate::account::{allocate_step, allocate_unpriced_step};
use crate::model::{CacheClass, Component, RunRecord};
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlamegraphNode {
    pub name: String,
    pub tokens: u64,
    pub micros: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_class: Option<CacheClass>,
    pub children: Vec<FlamegraphNode>,
}

impl FlamegraphNode {
    fn leaf(name: String, tokens: u64, micros: i64, class: CacheClass) -> Self {
        FlamegraphNode {
            name,
            tokens,
            micros,
            cache_class: Some(class),
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlamegraphModel {
    pub run_id: String,
    pub pricing_version: String,
    pub effective_date: String,
    pub root: FlamegraphNode,
}

/// One row of the flat/cum profile table — the pprof primitive: `self` is spend
/// attributed *directly* to a name (leaf spend; 0 for pure aggregator frames), `cum` is
/// self + all descendants. Aggregated by node name across the tree, so a name that recurs at
/// many steps (e.g. a cache class, a component) sums into one row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRow {
    pub name: String,
    /// Spend charged directly at nodes with this name (leaf micros; parent frames contribute 0).
    pub self_micros: i64,
    /// self + descendants across every occurrence of this name.
    pub cum_micros: i64,
    pub self_tokens: u64,
    pub cum_tokens: u64,
}

/// Sort key for the profile table: flat (self) or cumulative — like pprof's `-flat`/`-cum`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileSort {
    Flat,
    Cum,
}

/// The flat/cum profile table over a run's flamegraph. Sum of `self_micros` across
/// all rows equals `total_micros` (the pprof invariant — the root frame contributes 0 self and is
/// excluded); `cum_micros` can sum past the total because nesting is counted at every level.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileTable {
    pub run_id: String,
    pub pricing_version: String,
    pub sort: ProfileSort,
    /// Total run spend (denominator for percentages); equals the sum of every row's `self_micros`.
    pub total_micros: i64,
    pub total_tokens: u64,
    /// Rows sorted by the chosen key desc, tie-broken by name asc; truncated to `top_n` if given.
    pub rows: Vec<ProfileRow>,
}

/// Build the flat/cum profile table from an already-computed flamegraph — a pure re-sort of the
/// tree, no pricing lookups. `top_n` truncates after sorting (None = all rows).
pub fn profile_table(
    model: &FlamegraphModel,
    sort: ProfileSort,
    top_n: Option<usize>,
) -> ProfileTable {
    // Aggregate self/cum per name. `self` = a node's own micros minus its children's (0 for a
    // pure aggregator, = micros for a leaf); `cum` = the node's micros (already self+descendants).
    let mut agg: BTreeMap<String, (i64, i64, u64, u64)> = BTreeMap::new(); // name -> (self_m, cum_m, self_t, cum_t)
    fn walk(
        node: &FlamegraphNode,
        is_root: bool,
        agg: &mut BTreeMap<String, (i64, i64, u64, u64)>,
    ) {
        if !is_root {
            let child_micros = node
                .children
                .iter()
                .fold(0i64, |sum, child| sum.saturating_add(child.micros));
            let child_tokens = node
                .children
                .iter()
                .fold(0u64, |sum, child| sum.saturating_add(child.tokens));
            let self_micros = node.micros.saturating_sub(child_micros).max(0);
            let self_tokens = node.tokens.saturating_sub(child_tokens);
            let e = agg.entry(node.name.clone()).or_insert((0, 0, 0, 0));
            e.0 = e.0.saturating_add(self_micros);
            e.1 = e.1.saturating_add(node.micros);
            e.2 = e.2.saturating_add(self_tokens);
            e.3 = e.3.saturating_add(node.tokens);
        }
        for c in &node.children {
            walk(c, false, agg);
        }
    }
    walk(&model.root, true, &mut agg);

    let mut rows: Vec<ProfileRow> = agg
        .into_iter()
        .map(|(name, (sm, cm, st, ct))| ProfileRow {
            name,
            self_micros: sm,
            cum_micros: cm,
            self_tokens: st,
            cum_tokens: ct,
        })
        .collect();
    rows.sort_by(|a, b| {
        let key = |r: &ProfileRow| match sort {
            ProfileSort::Flat => r.self_micros,
            ProfileSort::Cum => r.cum_micros,
        };
        key(b).cmp(&key(a)).then(a.name.cmp(&b.name))
    });
    if let Some(n) = top_n {
        rows.truncate(n);
    }

    ProfileTable {
        run_id: model.run_id.clone(),
        pricing_version: model.pricing_version.clone(),
        sort,
        total_micros: model.root.micros,
        total_tokens: model.root.tokens,
        rows,
    }
}

/// Sort children by tokens desc, then name asc — byte-stable.
fn sort_children(children: &mut [FlamegraphNode]) {
    children.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.name.cmp(&b.name)));
}

pub fn build_flamegraph(run: &RunRecord, pricing: &PricingTable) -> FlamegraphModel {
    let mut step_nodes: Vec<FlamegraphNode> = Vec::new();

    let mut sorted_steps = run.steps.clone();
    sorted_steps.sort_by_key(|s| s.step_ordinal);

    for step in &sorted_steps {
        let allocs = match pricing.lookup(step.provider, step.shape.vendor.as_deref(), &step.model)
        {
            Some(rates) => allocate_step(&step.usage, rates, &step.shape),
            // Unknown model: preserve token structure with zero cost. The UI can then show an
            // honest token-weighted flame rather than an empty graph or a fabricated dollar value.
            None => allocate_unpriced_step(&step.usage, &step.shape),
        };

        // Group leaves by component.
        let mut by_comp: BTreeMap<Component, Vec<FlamegraphNode>> = BTreeMap::new();
        for a in &allocs {
            by_comp
                .entry(a.component)
                .or_default()
                .push(FlamegraphNode::leaf(
                    a.class.as_str().to_string(),
                    a.tokens,
                    a.micros.micros(),
                    a.class,
                ));
        }

        let mut comp_nodes: Vec<FlamegraphNode> = Vec::new();
        for (comp, mut leaves) in by_comp {
            sort_children(&mut leaves);
            let tokens = leaves
                .iter()
                .fold(0u64, |sum, leaf| sum.saturating_add(leaf.tokens));
            let micros = leaves
                .iter()
                .fold(0i64, |sum, leaf| sum.saturating_add(leaf.micros));
            comp_nodes.push(FlamegraphNode {
                name: comp.label().to_string(),
                tokens,
                micros,
                cache_class: None,
                children: leaves,
            });
        }
        sort_children(&mut comp_nodes);

        let tokens = comp_nodes
            .iter()
            .fold(0u64, |sum, component| sum.saturating_add(component.tokens));
        let micros = comp_nodes
            .iter()
            .fold(0i64, |sum, component| sum.saturating_add(component.micros));
        // Adapter step label (if present) makes the node self-describing; otherwise the name is
        // byte-identical to before, so label-less goldens are unchanged.
        let name = match &step.shape.step_label {
            Some(label) => format!("step {} · {} · {}", step.step_ordinal, step.model, label),
            None => format!("step {} · {}", step.step_ordinal, step.model),
        };
        step_nodes.push(FlamegraphNode {
            name,
            tokens,
            micros,
            cache_class: None,
            children: comp_nodes,
        });
    }

    // Steps stay in execution order under the root (ordinal); do not re-sort.
    let tokens = step_nodes
        .iter()
        .fold(0u64, |sum, step| sum.saturating_add(step.tokens));
    let micros = step_nodes
        .iter()
        .fold(0i64, |sum, step| sum.saturating_add(step.micros));

    FlamegraphModel {
        run_id: run.run_id.clone(),
        pricing_version: pricing.version.clone(),
        effective_date: pricing.effective_date.clone(),
        root: FlamegraphNode {
            name: format!("run {}", run.run_id),
            tokens,
            micros,
            cache_class: None,
            children: step_nodes,
        },
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    fn leaf(name: &str, tokens: u64, micros: i64) -> FlamegraphNode {
        FlamegraphNode {
            name: name.into(),
            tokens,
            micros,
            cache_class: Some(CacheClass::Output),
            children: vec![],
        }
    }
    fn node(name: &str, children: Vec<FlamegraphNode>) -> FlamegraphNode {
        let tokens = children.iter().map(|c| c.tokens).sum();
        let micros = children.iter().map(|c| c.micros).sum();
        FlamegraphNode {
            name: name.into(),
            tokens,
            micros,
            cache_class: None,
            children,
        }
    }

    // run → 2 steps, each with an "input" and "output" leaf. Names recur across steps so the
    // table aggregates them into one row apiece.
    fn model() -> FlamegraphModel {
        let step1 = node(
            "step 1 · m",
            vec![leaf("input", 100, 300), leaf("output", 50, 700)],
        );
        let step2 = node(
            "step 2 · m",
            vec![leaf("input", 200, 600), leaf("output", 10, 140)],
        );
        FlamegraphModel {
            run_id: "r".into(),
            pricing_version: "t".into(),
            effective_date: "2026-06-01".into(),
            root: node("run r", vec![step1, step2]),
        }
    }

    #[test]
    fn flat_sums_to_total_and_aggregates_by_name() {
        let m = model();
        let t = profile_table(&m, ProfileSort::Flat, None);
        // self across all rows == total (pprof invariant); root frame excluded.
        let self_sum: i64 = t.rows.iter().map(|r| r.self_micros).sum();
        assert_eq!(self_sum, t.total_micros);
        assert_eq!(t.total_micros, 300 + 700 + 600 + 140);
        // "input"/"output" leaves aggregate; step frames are pure aggregators (self = 0).
        let input = t.rows.iter().find(|r| r.name == "input").unwrap();
        assert_eq!(input.self_micros, 900, "300 + 600");
        assert_eq!(input.cum_micros, 900, "leaf: cum == self");
        let step1 = t.rows.iter().find(|r| r.name == "step 1 · m").unwrap();
        assert_eq!(
            step1.self_micros, 0,
            "aggregator frame charges nothing directly"
        );
        assert_eq!(step1.cum_micros, 1000, "300 + 700");
        // Flat sort: biggest self first → the two leaf rows lead.
        assert!(t.rows[0].self_micros >= t.rows[1].self_micros);
    }

    #[test]
    fn cum_sort_and_top_n_truncate() {
        let m = model();
        let t = profile_table(&m, ProfileSort::Cum, Some(2));
        assert_eq!(t.rows.len(), 2, "top-2");
        assert_eq!(t.sort, ProfileSort::Cum);
        // By cum, the step frames (1000, 740) and output(840)/input(900) compete; top row is the
        // largest cum. Ranked desc.
        assert!(t.rows[0].cum_micros >= t.rows[1].cum_micros);
        assert_eq!(
            t.rows[0].cum_micros, 1000,
            "step 1 has the largest cumulative"
        );
    }

    #[test]
    fn empty_run_yields_empty_table() {
        let m = FlamegraphModel {
            run_id: "r".into(),
            pricing_version: "t".into(),
            effective_date: "2026-06-01".into(),
            root: node("run r", vec![]),
        };
        let t = profile_table(&m, ProfileSort::Flat, None);
        assert!(t.rows.is_empty());
        assert_eq!(t.total_micros, 0);
    }

    #[test]
    fn unpriced_steps_remain_visible_with_tokens_and_zero_cost() {
        use crate::model::{Provider, RunRecord};

        let mut step = crate::ingest_step(
            "unpriced",
            1,
            Provider::Openai,
            include_bytes!("../../fixtures/openai_nonstream/request.json"),
            include_bytes!("../../fixtures/openai_nonstream/response.json"),
        )
        .unwrap();
        step.model = "future-unpriced-model".to_string();
        let expected_tokens = step.usage.total();
        let run = RunRecord {
            run_id: "unpriced".to_string(),
            steps: vec![step],
        };
        let pricing =
            PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml"))
                .unwrap();

        let flame = build_flamegraph(&run, &pricing);
        assert_eq!(flame.root.tokens, expected_tokens);
        assert_eq!(flame.root.micros, 0);
        assert_eq!(flame.root.children.len(), 1);
        assert!(!flame.root.children[0].children.is_empty());
        assert!(flame.root.children[0]
            .children
            .iter()
            .flat_map(|component| component.children.iter())
            .all(|leaf| leaf.micros == 0));
    }
}
