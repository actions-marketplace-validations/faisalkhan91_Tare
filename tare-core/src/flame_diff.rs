//! Flame-diff: the regression AT THE NODE. Merge two flamegraphs (a baseline `A` and
//! a current `B`) into one tree keyed by node name, carrying each side's cost and the delta
//! (`b − a`) so a renderer can paint it red where B got costlier and blue where it got cheaper —
//! the node-level companion to `diff.rs`'s row-level report.
//!
//! `--normalize` (share mode) compares each node's SHARE of its own tree total (in basis points)
//! instead of absolute dollars, so two different-sized runs diff *structurally* — "where did the
//! money go proportionally" rather than "which run was bigger". Pure, integer, deterministic.

use crate::flamegraph::{FlamegraphModel, FlamegraphNode};
use crate::model::CacheClass;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One merged node: A's and B's cost side by side, plus the signed delta.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlameDiffNode {
    pub name: String,
    pub tokens_a: u64,
    pub tokens_b: u64,
    pub micros_a: i64,
    pub micros_b: i64,
    /// `micros_b − micros_a`. Positive = costlier in B (red); negative = cheaper (blue).
    pub delta_micros: i64,
    /// Share of each tree's total in basis points (`micros × 10_000 / total`), and the delta —
    /// the axis a normalized view colors by. `delta_bps = share_b_bps − share_a_bps`.
    pub share_a_bps: i64,
    pub share_b_bps: i64,
    pub delta_bps: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_class: Option<CacheClass>,
    pub children: Vec<FlameDiffNode>,
}

/// The merged diff tree over two runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlameDiffModel {
    pub run_a: String,
    pub run_b: String,
    /// True when the caller asked for share-mode (structural) diffing.
    pub normalized: bool,
    pub total_a_micros: i64,
    pub total_b_micros: i64,
    pub root: FlameDiffNode,
}

fn bps(part: i64, total: i64) -> i64 {
    if total <= 0 {
        0
    } else {
        // Clamp the i128 result into i64 rather than a truncating `as` (module discipline).
        (part as i128 * 10_000 / total as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }
}

/// Merge two optional nodes (either side may be absent — a node present in only one run) into a
/// diff node, recursing over the union of child names (sorted, for determinism).
fn merge(
    a: Option<&FlamegraphNode>,
    b: Option<&FlamegraphNode>,
    total_a: i64,
    total_b: i64,
) -> FlameDiffNode {
    let name = a.or(b).map(|n| n.name.clone()).unwrap_or_default();
    let cache_class = a
        .and_then(|n| n.cache_class)
        .or_else(|| b.and_then(|n| n.cache_class));
    let (tokens_a, micros_a) = a.map(|n| (n.tokens, n.micros)).unwrap_or((0, 0));
    let (tokens_b, micros_b) = b.map(|n| (n.tokens, n.micros)).unwrap_or((0, 0));

    // Union of child names → recurse. BTreeMap keeps a deterministic (name-sorted) child order.
    let mut names: BTreeMap<&str, ()> = BTreeMap::new();
    if let Some(n) = a {
        for c in &n.children {
            names.insert(c.name.as_str(), ());
        }
    }
    if let Some(n) = b {
        for c in &n.children {
            names.insert(c.name.as_str(), ());
        }
    }
    let children: Vec<FlameDiffNode> = names
        .keys()
        .map(|nm| {
            let ca = a.and_then(|n| n.children.iter().find(|c| c.name == *nm));
            let cb = b.and_then(|n| n.children.iter().find(|c| c.name == *nm));
            merge(ca, cb, total_a, total_b)
        })
        .collect();

    let share_a_bps = bps(micros_a, total_a);
    let share_b_bps = bps(micros_b, total_b);
    FlameDiffNode {
        name,
        tokens_a,
        tokens_b,
        micros_a,
        micros_b,
        delta_micros: micros_b.saturating_sub(micros_a),
        share_a_bps,
        share_b_bps,
        delta_bps: share_b_bps.saturating_sub(share_a_bps),
        cache_class,
        children,
    }
}

/// Build the node-level diff between two flamegraphs. `normalize` only sets the `normalized` flag +
/// is what a renderer keys its color on (both absolute and share deltas are always computed).
pub fn flame_diff(a: &FlamegraphModel, b: &FlamegraphModel, normalize: bool) -> FlameDiffModel {
    let total_a = a.root.micros;
    let total_b = b.root.micros;
    FlameDiffModel {
        run_a: a.run_id.clone(),
        run_b: b.run_id.clone(),
        normalized: normalize,
        total_a_micros: total_a,
        total_b_micros: total_b,
        root: merge(Some(&a.root), Some(&b.root), total_a, total_b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flamegraph::build_flamegraph;
    use crate::{build_runs, ingest_step, model::Provider};

    fn pricing() -> crate::pricing::PricingTable {
        crate::pricing::PricingTable::from_toml_str(include_str!(
            "../../pricing/pricing.fixture.toml"
        ))
        .unwrap()
    }

    fn model_from(fixture: &str, run_id: &str) -> FlamegraphModel {
        // Reuse a captured fixture; both sides use the same shape so the diff is well-defined.
        let (req, resp): (&[u8], &[u8]) = match fixture {
            "bloat" => (
                include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json"),
                include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json"),
            ),
            _ => unreachable!(),
        };
        let step = ingest_step(run_id, 1, Provider::Anthropic, req, resp).unwrap();
        let runs = build_runs(vec![step]);
        build_flamegraph(&runs[0], &pricing())
    }

    #[test]
    fn diff_of_a_run_with_itself_is_all_zero_delta() {
        let m = model_from("bloat", "r1");
        let d = flame_diff(&m, &m, false);
        assert_eq!(d.total_a_micros, d.total_b_micros);
        assert_eq!(d.root.delta_micros, 0);
        assert_eq!(d.root.delta_bps, 0);
        // Every node has matching a/b and zero delta.
        fn all_zero(n: &FlameDiffNode) -> bool {
            n.delta_micros == 0 && n.micros_a == n.micros_b && n.children.iter().all(all_zero)
        }
        assert!(all_zero(&d.root));
    }

    #[test]
    fn delta_is_b_minus_a_and_normalizes_to_shares() {
        // Build B as a doubled version of A by summing two copies of the run into one flamegraph.
        let a = model_from("bloat", "r1");
        let step2 = {
            let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
            let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
            let s1 = ingest_step("r2", 1, Provider::Anthropic, req, resp).unwrap();
            let s2 = ingest_step("r2", 2, Provider::Anthropic, req, resp).unwrap();
            build_flamegraph(&build_runs(vec![s1, s2])[0], &pricing())
        };
        let b = step2;
        assert!(b.root.micros > a.root.micros, "B is the larger run");

        let d = flame_diff(&a, &b, true);
        assert!(d.normalized);
        assert_eq!(d.root.delta_micros, b.root.micros - a.root.micros);
        assert!(d.root.delta_micros > 0, "B costlier at the root");
        // Root is 100% of each tree, so its SHARE delta is ~0 even though the absolute delta is big
        // — exactly the structural-vs-absolute distinction --normalize exists for.
        assert_eq!(d.root.share_a_bps, 10_000);
        assert_eq!(d.root.share_b_bps, 10_000);
        assert_eq!(d.root.delta_bps, 0);
    }
}
