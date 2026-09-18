//! Budget goals & streaks: the daily-return habit surface. Given a dense daily series
//! of spend + input-token classes and a set of user goals ("stay under $X/day", "cache-read ratio
//! ≥ N%"), compute per-day pass/fail and a WakaTime/Duolingo-style streak counter.
//!
//! Counts-only, NO judgment: a goal is the user's own threshold, and a met/missed day is a plain
//! comparison — never a score or a nag. Pure, integer, clock-free (the "today" boundary lives in the
//! caller that builds the series); the streak is defined purely by the order of the days passed in.

use serde::{Deserialize, Serialize};

/// The user's daily goals. Every field is optional; a `None` goal is simply not evaluated. Money is
/// integer micro-USD and the ratio is an integer percent, so nothing here is an `f64`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goals {
    /// "Stay at or under $X/day" — the per-day spend ceiling in micro-USD.
    pub max_daily_micros: Option<i64>,
    /// "Cache-read ratio ≥ N%" — the minimum share of input tokens served from cache.
    pub min_cache_read_pct: Option<i64>,
}

impl Goals {
    /// True if at least one goal is set (otherwise a streak is meaningless).
    pub fn any_set(&self) -> bool {
        self.max_daily_micros.is_some() || self.min_cache_read_pct.is_some()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_daily_micros.is_some_and(|value| value < 0) {
            return Err("max daily spend must be non-negative".into());
        }
        if self
            .min_cache_read_pct
            .is_some_and(|value| !(0..=100).contains(&value))
        {
            return Err("minimum cache-read percent must be between 0 and 100".into());
        }
        Ok(())
    }
}

/// One day's pre-aggregated figures (built by the caller from the stored, dated runs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayStats {
    pub date: String,
    pub micros: i64,
    /// Input tokens served from cache (`cache_read`).
    pub cache_read_tokens: u64,
    /// All input tokens (`fresh_input + cache_read + cache_write_5m + cache_write_1h`) — the ratio
    /// denominator. Output/reasoning are excluded (the cache axis is an input-side concept).
    pub input_total_tokens: u64,
    /// True if the day includes spend on an UNPRICED model (excluded from `micros`). Then `micros`
    /// is an undercount, so a budget goal cannot be judged honestly for the day.
    #[serde(default)]
    pub has_unpriced: bool,
}

/// Per-day evaluation against the goals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayResult {
    pub date: String,
    pub micros: i64,
    /// `cache_read_tokens × 100 / input_total_tokens` (0 when there was no input).
    pub cache_read_pct: i64,
    /// `Some(true/false)` when a budget goal is set AND the day is fully priced; `None` when there's
    /// no budget goal, or the day has unpriced spend (see `budget_unknown`).
    pub under_budget: Option<bool>,
    /// A budget goal is set but the day includes unpriced spend, so "under budget" can't be judged
    /// because spend is undercounted. Such a day is never counted as met and cannot extend a streak.
    #[serde(default)]
    pub budget_unknown: bool,
    /// `Some(true/false)` when a cache-ratio goal is set; `None` otherwise. A day with no input
    /// tokens passes vacuously (you can't miss a cache ratio with no requests).
    pub met_cache: Option<bool>,
    /// All SET goals met on this day (false when no goals are set — nothing to be on a streak for).
    pub met_all: bool,
}

/// The streak report: per-day results plus the current + longest consecutive met-all runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreakReport {
    pub days: Vec<DayResult>,
    /// Consecutive met-all days counting back from the most recent day (0 if the last day missed).
    pub current_streak: u32,
    /// Longest consecutive met-all run anywhere in the series.
    pub longest_streak: u32,
    pub goals: Goals,
}

/// Evaluate a chronologically-ascending daily series against the goals. Pure + deterministic.
pub fn evaluate_streaks(days: &[DayStats], goals: &Goals) -> StreakReport {
    let any = goals.any_set();
    let results: Vec<DayResult> = days
        .iter()
        .map(|d| {
            let pct = if d.input_total_tokens == 0 {
                0
            } else {
                let read = d.cache_read_tokens.min(d.input_total_tokens);
                (u128::from(read) * 100 / u128::from(d.input_total_tokens)) as i64
            };
            // Budget: a set goal is only judged on a fully-priced day. If the day has unpriced spend
            // its `micros` is an under-count, so we mark it UNKNOWN (never silently "under budget").
            let budget_unknown = goals.max_daily_micros.is_some() && d.has_unpriced;
            let under_budget = match goals.max_daily_micros {
                Some(_) if d.has_unpriced => None,
                Some(m) => Some(d.micros <= m),
                None => None,
            };
            let met_cache = goals
                .min_cache_read_pct
                .map(|t| d.input_total_tokens == 0 || pct >= t);
            // Each SET goal must be definitively met: a budget goal that's unknown (unpriced) or over
            // is NOT met; an unset goal isn't a constraint. Prevents an unverifiable day extending a streak.
            let budget_met = match goals.max_daily_micros {
                Some(_) => under_budget == Some(true),
                None => true,
            };
            let met_all = any && budget_met && met_cache.unwrap_or(true);
            DayResult {
                date: d.date.clone(),
                micros: d.micros,
                cache_read_pct: pct,
                under_budget,
                budget_unknown,
                met_cache,
                met_all,
            }
        })
        .collect();

    // Longest run anywhere; current run counts back from the end.
    let mut longest = 0u32;
    let mut run = 0u32;
    for r in &results {
        if r.met_all {
            run = run.saturating_add(1);
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let mut current = 0u32;
    for r in results.iter().rev() {
        if r.met_all {
            current = current.saturating_add(1);
        } else {
            break;
        }
    }

    StreakReport {
        days: results,
        current_streak: current,
        longest_streak: longest,
        goals: goals.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, micros: i64, read: u64, total: u64) -> DayStats {
        DayStats {
            date: date.into(),
            micros,
            cache_read_tokens: read,
            input_total_tokens: total,
            has_unpriced: false,
        }
    }

    #[test]
    fn unpriced_day_is_unknown_not_under_budget_and_breaks_the_streak() {
        let goals = Goals {
            max_daily_micros: Some(1_000_000),
            min_cache_read_pct: None,
        };
        let mut d2 = day("2026-06-21", 100_000, 0, 0); // well under $1, BUT has unpriced spend
        d2.has_unpriced = true;
        let days = vec![
            day("2026-06-20", 500_000, 0, 0),
            d2,
            day("2026-06-22", 200_000, 0, 0),
        ];
        let r = evaluate_streaks(&days, &goals);
        // The unpriced day can't be judged: unknown, not met, breaks the run.
        assert_eq!(r.days[1].under_budget, None);
        assert!(r.days[1].budget_unknown);
        assert!(!r.days[1].met_all);
        assert_eq!(r.current_streak, 1, "only the last (priced, under) day");
        assert_eq!(r.longest_streak, 1);
    }

    #[test]
    fn budget_goal_streak_counts_recent_consecutive_days() {
        let goals = Goals {
            max_daily_micros: Some(1_000_000), // $1/day
            min_cache_read_pct: None,
        };
        // under, under, OVER, under, under -> current streak 2, longest 2.
        let days = vec![
            day("2026-06-20", 500_000, 0, 0),
            day("2026-06-21", 900_000, 0, 0),
            day("2026-06-22", 2_000_000, 0, 0),
            day("2026-06-23", 100_000, 0, 0),
            day("2026-06-24", 999_999, 0, 0),
        ];
        let r = evaluate_streaks(&days, &goals);
        assert_eq!(r.current_streak, 2);
        assert_eq!(r.longest_streak, 2);
        assert_eq!(r.days[2].under_budget, Some(false));
        assert_eq!(r.days[2].met_cache, None, "no cache goal set");
    }

    #[test]
    fn cache_ratio_goal_and_vacuous_idle_days() {
        let goals = Goals {
            max_daily_micros: None,
            min_cache_read_pct: Some(50),
        };
        let days = vec![
            day("2026-06-20", 100, 800, 1000), // 80% >= 50 -> met
            day("2026-06-21", 0, 0, 0),        // idle -> vacuously met
            day("2026-06-22", 100, 100, 1000), // 10% < 50 -> miss
            day("2026-06-23", 100, 900, 1000), // 90% -> met
        ];
        let r = evaluate_streaks(&days, &goals);
        assert_eq!(r.days[0].cache_read_pct, 80);
        assert_eq!(r.days[0].met_cache, Some(true));
        assert_eq!(r.days[1].met_cache, Some(true), "idle day passes vacuously");
        assert_eq!(r.days[2].met_cache, Some(false));
        assert_eq!(r.current_streak, 1, "only the last day since the miss");
        assert_eq!(r.longest_streak, 2, "days 0+1");
    }

    #[test]
    fn both_goals_must_hold_and_no_goals_is_no_streak() {
        let goals = Goals {
            max_daily_micros: Some(1_000_000),
            min_cache_read_pct: Some(50),
        };
        // Under budget but low cache -> misses (both must hold).
        let days = vec![day("2026-06-20", 500_000, 100, 1000)];
        let r = evaluate_streaks(&days, &goals);
        assert!(!r.days[0].met_all);
        assert_eq!(r.current_streak, 0);

        // No goals -> nothing to streak on.
        let none = evaluate_streaks(&days, &Goals::default());
        assert!(!none.days[0].met_all);
        assert_eq!(none.current_streak, 0);
    }

    #[test]
    fn validates_goals_and_clamps_inconsistent_cache_counts() {
        assert!(Goals {
            max_daily_micros: Some(-1),
            min_cache_read_pct: None,
        }
        .validate()
        .is_err());
        assert!(Goals {
            max_daily_micros: None,
            min_cache_read_pct: Some(101),
        }
        .validate()
        .is_err());

        let goals = Goals {
            max_daily_micros: None,
            min_cache_read_pct: Some(100),
        };
        let report = evaluate_streaks(&[day("2026-06-20", 0, u64::MAX, 1)], &goals);
        assert_eq!(report.days[0].cache_read_pct, 100);
        assert_eq!(report.days[0].met_cache, Some(true));
    }
}
