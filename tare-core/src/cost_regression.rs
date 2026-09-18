//! Outcome-aware cost-effectiveness regression detector. Where `anomaly.rs` alarms on
//! raw spend, this alarms on UNIT cost — the $/outcome ratio ($/commit, else $/accepted-edit) —
//! deviating from its own trailing trend. A day where spend doubles AND commits double is fine;
//! a day where $/commit jumps is the FinOps alarm. Pure, integer, clock-free: the caller supplies
//! the per-day series; detection reuses the same trailing-median-plus-threshold shape as anomaly.rs.

use serde::{Deserialize, Serialize};

use crate::money::scaled_div;

/// One day's cost + outcome counters (assembled by the store from the metered series).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayOutcome {
    pub day: String,
    pub cost_micros: i64,
    pub commits: u64,
    pub edits_accepted: u64,
}

/// A flagged day whose unit cost regressed above its trailing baseline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostRegression {
    pub day: String,
    /// Which denominator was used: `"commit"` or `"accepted-edit"`.
    pub outcome: String,
    /// This day's $/outcome (micro-USD per outcome).
    pub ratio_micros: i64,
    /// The trailing-window median $/outcome it's compared against.
    pub baseline_micros: i64,
    /// Percent above baseline (e.g. 140 = 140% over = 2.4× the baseline).
    pub over_pct: i64,
}

/// The denominator to use: commits if any day has a commit, else accepted-edits. Returns `None`
/// when neither outcome is ever present (nothing to ratio against).
fn pick_outcome(days: &[DayOutcome]) -> Option<&'static str> {
    if days.iter().any(|d| d.commits > 0) {
        Some("commit")
    } else if days.iter().any(|d| d.edits_accepted > 0) {
        Some("accepted-edit")
    } else {
        None
    }
}

fn count(d: &DayOutcome, outcome: &str) -> u64 {
    match outcome {
        "commit" => d.commits,
        _ => d.edits_accepted,
    }
}

fn median(mut v: Vec<i64>) -> i64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

/// Flag days whose $/outcome exceeds the trailing-window median by more than `threshold_pct`
/// (negative values are treated as zero, matching `anomaly::detect`). `window` = trailing days
/// considered for the baseline (only days that HAVE a
/// ratio count toward it — so it spans the last N days-with-activity, not N calendar days). Days
/// with a zero outcome are skipped (an undefined ratio, never a regression). Sorted by `over_pct`
/// desc then day. Deterministic.
pub fn detect(days: &[DayOutcome], window: usize, threshold_pct: i64) -> Vec<CostRegression> {
    let Some(outcome) = pick_outcome(days) else {
        return Vec::new();
    };
    let window = window.max(1);
    let threshold_pct = threshold_pct.max(0);
    // Per-day ratio (micro-USD / outcome), keeping the day + its index; None where outcome == 0.
    let ratios: Vec<Option<i64>> = days
        .iter()
        .map(|d| {
            let n = count(d, outcome);
            // A day is ratio-able iff it produced the outcome; a free-but-productive day (cost 0)
            // is a legitimate low ratio that belongs in the baseline, so gate only on the outcome.
            // Negative store sums (unclamped cost) floor to 0 rather than yielding a negative ratio.
            scaled_div(d.cost_micros.max(0), 1, n)
        })
        .collect();

    let mut out = Vec::new();
    for (i, r) in ratios.iter().enumerate() {
        let Some(ratio) = *r else { continue };
        // Trailing baseline: the median of the prior `window` days that HAD a ratio.
        let prior: Vec<i64> = ratios[..i]
            .iter()
            .rev()
            .filter_map(|x| *x)
            .take(window)
            .collect();
        if prior.is_empty() {
            continue; // no trend yet — can't call a regression
        }
        let baseline = median(prior);
        if baseline <= 0 {
            continue;
        }
        if (ratio as i128) * 100 > (baseline as i128) * (100 + threshold_pct as i128) {
            let over_pct =
                scaled_div(ratio.saturating_sub(baseline), 100, baseline as u64).unwrap_or(0);
            out.push(CostRegression {
                day: days[i].day.clone(),
                outcome: outcome.to_string(),
                ratio_micros: ratio,
                baseline_micros: baseline,
                over_pct,
            });
        }
    }
    out.sort_by(|a, b| b.over_pct.cmp(&a.over_pct).then(a.day.cmp(&b.day)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: &str, cost: i64, commits: u64) -> DayOutcome {
        DayOutcome {
            day: d.into(),
            cost_micros: cost,
            commits,
            edits_accepted: 0,
        }
    }

    #[test]
    fn flags_unit_cost_spike_not_proportional_growth() {
        // Days 1-3: steady $1/commit. Day 4: spend AND commits both double -> $1/commit, fine.
        // Day 5: $/commit jumps to $3 -> a regression.
        let days = vec![
            day("d1", 1_000_000, 1),
            day("d2", 2_000_000, 2),
            day("d3", 1_000_000, 1),
            day("d4", 4_000_000, 4), // still $1/commit despite 4× spend
            day("d5", 3_000_000, 1), // $3/commit — 200% over the $1 baseline
        ];
        let regs = detect(&days, 3, 50);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].day, "d5");
        assert_eq!(regs[0].outcome, "commit");
        assert_eq!(regs[0].ratio_micros, 3_000_000);
        assert_eq!(regs[0].baseline_micros, 1_000_000);
        assert_eq!(regs[0].over_pct, 200);
    }

    #[test]
    fn no_outcomes_means_no_regressions() {
        let days = vec![DayOutcome {
            day: "d1".into(),
            cost_micros: 5_000_000,
            commits: 0,
            edits_accepted: 0,
        }];
        assert!(detect(&days, 7, 50).is_empty());
    }

    #[test]
    fn zero_cost_productive_day_counts_in_the_baseline() {
        // A free-but-productive day (cost 0, 1 commit) is a real $0/commit point: it must lower the
        // trailing baseline, not be dropped. Two free days then a $1/commit day is a regression
        // over the $0 baseline only if baseline > 0 — here baseline is 0 so it can't be a percent
        // spike; but a later day must see the free days reflected in its median, not skipped.
        let days = vec![
            day("d1", 0, 1),         // $0/commit
            day("d2", 1_000_000, 1), // $1
            day("d3", 1_000_000, 1), // $1
            day("d4", 4_000_000, 1), // $4 — median of [$0,$1,$1] = $1, 300% over
        ];
        let regs = detect(&days, 3, 50);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].day, "d4");
        assert_eq!(regs[0].baseline_micros, 1_000_000);
        assert_eq!(regs[0].over_pct, 300);
    }

    #[test]
    fn falls_back_to_accepted_edits_when_no_commits() {
        let days = vec![
            DayOutcome {
                day: "d1".into(),
                cost_micros: 1_000_000,
                commits: 0,
                edits_accepted: 10,
            },
            DayOutcome {
                day: "d2".into(),
                cost_micros: 5_000_000,
                commits: 0,
                edits_accepted: 10,
            },
        ];
        let regs = detect(&days, 7, 50);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].outcome, "accepted-edit");
    }

    #[test]
    fn baseline_spans_the_last_active_days_across_gaps() {
        let days = vec![
            day("d1", 1_000_000, 1),
            day("d2", 0, 0),
            day("d3", 0, 0),
            day("d4", 3_000_000, 1),
        ];
        let regs = detect(&days, 2, 50);
        assert_eq!(regs.len(), 1);
        assert_eq!(regs[0].day, "d4");
        assert_eq!(regs[0].baseline_micros, 1_000_000);
    }

    #[test]
    fn extreme_counts_and_percentages_do_not_wrap() {
        let days = vec![day("d1", 1, 1), day("d2", i64::MAX, 1)];
        let regs = detect(&days, 1, 0);
        assert_eq!(regs[0].over_pct, i64::MAX);
        assert_eq!(
            detect(&[day("d1", 100, u64::MAX)], 1, 0),
            Vec::<CostRegression>::new()
        );
    }
}
