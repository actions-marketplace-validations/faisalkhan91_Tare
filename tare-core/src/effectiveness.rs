//! Cost-EFFECTIVENESS: the value side of the equation. Tare measures the denominator
//! (token cost) precisely; this divides it by the OUTCOME counters Claude Code emits (and Tare now
//! captures into the metered lane) to answer "what did the spend BUY?" — dollars per
//! merged PR, per commit, per 1k lines, per active hour, per session, plus the edit accept-rate.
//!
//! Pure: it takes a plain `OutcomeCounts` (assembled by the store/CLI from the metered series over
//! a window) so core stays free of storage + clock. Every ratio is `None` when its denominator is
//! zero (no divide-by-zero, no fake "$0/PR"). Estimate-only, like everything in Tare.

use serde::{Deserialize, Serialize};

use crate::money::scaled_div;

/// Window totals from the metered series: cost + the outcome counters it's divided by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeCounts {
    pub cost_micros: i64,
    pub pull_requests: u64,
    pub commits: u64,
    pub lines_added: u64,
    pub active_seconds: u64,
    pub sessions: u64,
    pub edits_accepted: u64,
    pub edits_rejected: u64,
}

/// Cost-effectiveness ratios (micro-USD), each `None` when its denominator is zero.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effectiveness {
    pub cost_micros: i64,
    /// $/merged-PR.
    pub per_pull_request_micros: Option<i64>,
    /// $/commit.
    pub per_commit_micros: Option<i64>,
    /// $/1,000 lines added.
    pub per_1k_loc_micros: Option<i64>,
    /// $/active hour (active_time seconds / 3600).
    pub per_active_hour_micros: Option<i64>,
    /// $/CLI session.
    pub per_session_micros: Option<i64>,
    /// Edit accept-rate, integer percent (accepted / (accepted+rejected)).
    pub accept_rate_pct: Option<i64>,
    /// Cost per SUCCESSFUL run — set by the caller from the run-outcome split, since it
    /// needs the runs (not the metered series). `None` until populated / when no run succeeded.
    #[serde(default)]
    pub cost_per_successful_run_micros: Option<i64>,
    /// Run success rate, integer percent (set alongside the above).
    #[serde(default)]
    pub run_success_rate_pct: Option<i64>,
    pub estimated: bool,
}

fn div(cost: i64, denom: u64) -> Option<i64> {
    scaled_div(cost, 1, denom)
}

/// Compute cost-effectiveness ratios from window outcome counts. Pure, integer, guards every
/// denominator.
pub fn effectiveness(c: &OutcomeCounts) -> Effectiveness {
    let decided = u128::from(c.edits_accepted) + u128::from(c.edits_rejected);
    Effectiveness {
        cost_micros: c.cost_micros,
        per_pull_request_micros: div(c.cost_micros, c.pull_requests),
        per_commit_micros: div(c.cost_micros, c.commits),
        // cost per 1k lines = cost / (lines/1000) = cost*1000/lines.
        per_1k_loc_micros: scaled_div(c.cost_micros, 1000, c.lines_added),
        // cost per hour = cost / (seconds/3600) = cost*3600/seconds.
        per_active_hour_micros: scaled_div(c.cost_micros, 3600, c.active_seconds),
        per_session_micros: div(c.cost_micros, c.sessions),
        accept_rate_pct: (decided > 0)
            .then(|| (u128::from(c.edits_accepted) * 100 / decided) as i64),
        // Populated by the caller from the run-outcome split (needs runs, not metered counts).
        cost_per_successful_run_micros: None,
        run_success_rate_pct: None,
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_cost_by_each_outcome() {
        let c = OutcomeCounts {
            cost_micros: 12_000_000, // $12
            pull_requests: 3,
            commits: 6,
            lines_added: 2000,
            active_seconds: 7200, // 2h
            sessions: 4,
            edits_accepted: 9,
            edits_rejected: 1,
        };
        let e = effectiveness(&c);
        assert_eq!(e.per_pull_request_micros, Some(4_000_000)); // $4/PR
        assert_eq!(e.per_commit_micros, Some(2_000_000)); // $2/commit
        assert_eq!(e.per_1k_loc_micros, Some(6_000_000)); // $12 over 2k lines = $6/1k
        assert_eq!(e.per_active_hour_micros, Some(6_000_000)); // $12 over 2h = $6/h
        assert_eq!(e.per_session_micros, Some(3_000_000)); // $3/session
        assert_eq!(e.accept_rate_pct, Some(90)); // 9/10
    }

    #[test]
    fn zero_denominators_are_none_not_zero() {
        let e = effectiveness(&OutcomeCounts {
            cost_micros: 5_000_000,
            ..Default::default()
        });
        assert_eq!(e.per_pull_request_micros, None);
        assert_eq!(e.per_commit_micros, None);
        assert_eq!(e.per_1k_loc_micros, None);
        assert_eq!(e.per_active_hour_micros, None);
        assert_eq!(e.accept_rate_pct, None);
        assert_eq!(e.cost_micros, 5_000_000);
    }

    #[test]
    fn extreme_counts_do_not_wrap_or_distort_ratios() {
        let e = effectiveness(&OutcomeCounts {
            cost_micros: i64::MAX,
            pull_requests: u64::MAX,
            lines_added: 1,
            active_seconds: 1,
            edits_accepted: u64::MAX,
            edits_rejected: u64::MAX,
            ..Default::default()
        });
        assert_eq!(e.per_pull_request_micros, Some(0));
        assert_eq!(e.per_1k_loc_micros, Some(i64::MAX));
        assert_eq!(e.per_active_hour_micros, Some(i64::MAX));
        assert_eq!(e.accept_rate_pct, Some(50));
    }
}
