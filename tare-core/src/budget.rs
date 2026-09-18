//! Budgets + runaway-loop kill-switch. A pure decision function the proxy consults
//! BEFORE forwarding each request, against a per-run tally. No clock/RNG.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Per-run limits. Any unset field is unenforced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Hard cap on estimated spend (micro-USD): forwarding is KILLED at/above this.
    pub max_micros: Option<i64>,
    /// Soft (warn-only) spend line (micro-USD): WARN at/above this but keep forwarding.
    /// Lets a user run "warn at $2, never block" or "warn at $2, kill at $5". When unset, the warn
    /// line defaults to 80% of `max_micros` (the historical behaviour).
    #[serde(default)]
    pub soft_micros: Option<i64>,
    /// Hard cap on the number of forwarded steps in the run.
    pub max_steps: Option<u32>,
    /// Runaway-loop guard: max times the SAME request may be issued in a run.
    pub max_identical_repeats: Option<u32>,
}

impl Budget {
    pub fn is_set(&self) -> bool {
        self.max_micros.is_some_and(|value| value > 0)
            || self.soft_micros.is_some_and(|value| value > 0)
            || self.max_steps.is_some()
            || self.max_identical_repeats.is_some()
    }
}

/// Mutable per-run running totals.
#[derive(Clone, Debug, Default)]
pub struct RunTally {
    pub micros: i64,
    /// Pre-send spend reserved for in-flight requests not yet costed. The spend cap is
    /// evaluated against `micros + reserved`, so concurrent forwards can't collectively
    /// overshoot `max_micros` by more than one in-flight step's estimation error.
    pub reserved: i64,
    pub steps: u32,
    pub repeats: BTreeMap<u64, u32>,
    /// Last budget `Decision` seen for this run, so alerts can fire ONCE on a strict
    /// escalation (Allow→Warn→Kill) rather than on every step. `None` before the first step.
    pub last_decision: Option<Decision>,
}

impl RunTally {
    /// Effective spend for the cap: settled cost plus in-flight reservations.
    pub fn effective_micros(&self) -> i64 {
        self.micros.max(0).saturating_add(self.reserved.max(0))
    }
    /// Record that an allowed request was forwarded.
    pub fn note_forwarded(&mut self, request_hash: u64) {
        let repeats = self.repeats.entry(request_hash).or_insert(0);
        *repeats = repeats.saturating_add(1);
        self.steps = self.steps.saturating_add(1);
    }
    /// Reserve an estimated cost for a request just forwarded (released on completion).
    pub fn reserve(&mut self, micros: i64) {
        self.reserved = self.reserved.max(0).saturating_add(micros.max(0));
    }
    /// Release a previously-made reservation (clamped at zero so it can't go negative).
    pub fn release(&mut self, micros: i64) {
        self.reserved = self.reserved.max(0).saturating_sub(micros.max(0));
    }
    /// Add a captured step's estimated cost. Saturating, like `reserve`/`effective_micros`: the
    /// running spend accumulator is bumped on every captured step, so a pathological cumulative
    /// total must clamp at the ceiling rather than panic (debug overflow) or wrap (release) — the
    /// same overflow-safe money invariant the rest of the cost path (account.rs i128+clamp) upholds.
    pub fn add_cost(&mut self, micros: i64) {
        self.micros = self.micros.max(0).saturating_add(micros.max(0));
    }
}

/// Backward-looking spend budget over a calendar window (weekly/monthly), distinct from the
/// per-run kill-switch: it answers "how much of my periodic budget is gone, and should I be
/// warned yet?" Pure — the caller supplies the already-summed period spend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeriodBudget {
    /// "week" | "month" (label only; the caller decides the window when summing spend).
    pub period: String,
    pub spent_micros: i64,
    pub cap_micros: i64,
    pub warn_pct: i64,
    /// Spend as a percent of cap (0+, integer; can exceed 100).
    pub pct: i64,
    /// "ok" | "warn" | "over".
    pub status: String,
}

/// Classify period spend against a cap with a pre-cap warn threshold (default 80%).
pub fn period_status(
    period: &str,
    spent_micros: i64,
    cap_micros: i64,
    warn_pct: i64,
) -> PeriodBudget {
    let warn = if warn_pct <= 0 { 80 } else { warn_pct.min(100) };
    let spent_micros = spent_micros.max(0);
    let pct = if cap_micros > 0 {
        ((i128::from(spent_micros) * 100 / i128::from(cap_micros)).min(i128::from(i64::MAX))) as i64
    } else {
        0
    };
    let status = if cap_micros <= 0 {
        "ok"
    } else if pct >= 100 {
        "over"
    } else if pct >= warn {
        "warn"
    } else {
        "ok"
    };
    PeriodBudget {
        period: period.to_string(),
        spent_micros,
        cap_micros,
        warn_pct: warn,
        pct,
        status: status.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Warn(String),
    Kill(String),
}

impl Decision {
    /// A stable, opaque code for the wire — NEVER the free-text reason (which could embed
    /// model- or prompt-derived text). Only this coded label crosses the proxy boundary.
    pub fn code(&self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Warn(_) => "warn",
            Decision::Kill(_) => "kill",
        }
    }
}

/// Decide whether an incoming request (identified by its canonical hash) may be
/// forwarded, given the budget and the run's tally SO FAR.
pub fn evaluate(budget: &Budget, tally: &RunTally, incoming_hash: u64) -> Decision {
    if let Some(max) = budget.max_identical_repeats {
        let current = tally.repeats.get(&incoming_hash).copied().unwrap_or(0);
        if current >= max {
            let next = u64::from(current) + 1;
            return Decision::Kill(format!(
                "runaway loop: identical request would be issued {next} times (limit {max})"
            ));
        }
    }
    if let Some(max) = budget.max_steps {
        if tally.steps >= max {
            return Decision::Kill(format!("step limit reached ({max})"));
        }
    }
    // Count in-flight reservations so concurrent forwards can't overshoot the cap.
    let spend = tally.effective_micros();
    // HARD cap kills (only when set).
    if let Some(max) = budget.max_micros.filter(|value| *value > 0) {
        if spend >= max {
            return Decision::Kill(format!("spend limit reached ({} micro-USD)", max));
        }
    }
    // SOFT line warns but keeps forwarding: an explicit soft_micros, else 80% of the
    // hard cap. This lets "warn at $2, never block" (soft set, no max) and "warn then kill".
    let warn_line = budget.soft_micros.filter(|value| *value > 0).or_else(|| {
        budget
            .max_micros
            .filter(|value| *value > 0)
            .map(|value| (i128::from(value) * 80 / 100) as i64)
    });
    if let Some(soft) = warn_line {
        if spend >= soft {
            return Decision::Warn(format!(
                "approaching spend ({} micro-USD; warn line {})",
                spend, soft
            ));
        }
    }
    Decision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runaway_loop_killed_on_repeat() {
        let b = Budget {
            max_identical_repeats: Some(2),
            ..Default::default()
        };
        let mut t = RunTally::default();
        assert_eq!(evaluate(&b, &t, 7), Decision::Allow);
        t.note_forwarded(7);
        assert_eq!(evaluate(&b, &t, 7), Decision::Allow); // 2nd is fine
        t.note_forwarded(7);
        // 3rd identical -> kill
        assert!(matches!(evaluate(&b, &t, 7), Decision::Kill(_)));
        // a different request is still allowed
        assert_eq!(evaluate(&b, &t, 9), Decision::Allow);
    }

    #[test]
    fn spend_warn_then_kill() {
        let b = Budget {
            max_micros: Some(1000),
            ..Default::default()
        };
        let mut t = RunTally::default();
        assert_eq!(evaluate(&b, &t, 1), Decision::Allow);
        t.add_cost(800); // 80%
        assert!(matches!(evaluate(&b, &t, 1), Decision::Warn(_)));
        t.add_cost(200); // 100%
        assert!(matches!(evaluate(&b, &t, 1), Decision::Kill(_)));
    }

    #[test]
    fn soft_line_warns_without_killing() {
        // "warn at $0.002, never block": soft set, no hard cap.
        let b = Budget {
            soft_micros: Some(2000),
            ..Default::default()
        };
        let mut t = RunTally::default();
        assert_eq!(evaluate(&b, &t, 1), Decision::Allow);
        t.add_cost(2500); // past the soft line
        assert!(matches!(evaluate(&b, &t, 1), Decision::Warn(_)));
        // No hard cap -> it NEVER escalates to Kill no matter how far over.
        t.add_cost(1_000_000);
        assert!(matches!(evaluate(&b, &t, 1), Decision::Warn(_)));
    }

    #[test]
    fn explicit_soft_line_overrides_the_default_80pct() {
        // warn at $0.002, kill at $0.005: warns at the soft line, not at 80% of the cap.
        let b = Budget {
            soft_micros: Some(2000),
            max_micros: Some(5000),
            ..Default::default()
        };
        let mut t = RunTally::default();
        t.add_cost(2000);
        assert!(
            matches!(evaluate(&b, &t, 1), Decision::Warn(_)),
            "warns at the soft line"
        );
        t.add_cost(3000); // 5000 = hard cap
        assert!(matches!(evaluate(&b, &t, 1), Decision::Kill(_)));
    }

    #[test]
    fn reservation_bounds_concurrent_overshoot() {
        // Two requests forward "simultaneously" before either is costed. Without a
        // reservation both would see micros=0 and pass; the reservation makes the second
        // observe the first's pending spend and trip the cap.
        let b = Budget {
            max_micros: Some(1000),
            ..Default::default()
        };
        let mut t = RunTally::default();
        // First request forwards and reserves ~800.
        assert!(matches!(
            evaluate(&b, &t, 1),
            Decision::Warn(_) | Decision::Allow
        ));
        t.note_forwarded(1);
        t.reserve(800);
        // Second concurrent request now sees effective spend 800 -> warn (>=80%).
        assert!(matches!(evaluate(&b, &t, 2), Decision::Warn(_)));
        t.note_forwarded(2);
        t.reserve(800); // effective 1600 >= cap
        assert!(matches!(evaluate(&b, &t, 3), Decision::Kill(_)));
        // Requests complete: reservations released, actual costs (say 600 total) settled.
        t.release(800);
        t.release(800);
        t.add_cost(600);
        assert_eq!(t.effective_micros(), 600);
        assert!(matches!(evaluate(&b, &t, 4), Decision::Allow));
    }

    #[test]
    fn period_status_classifies_ok_warn_over() {
        assert_eq!(period_status("month", 500_000, 1_000_000, 80).status, "ok"); // 50%
        assert_eq!(
            period_status("month", 850_000, 1_000_000, 80).status,
            "warn"
        ); // 85% >= warn
        assert_eq!(
            period_status("month", 1_200_000, 1_000_000, 80).status,
            "over"
        ); // 120%
        assert_eq!(period_status("month", 999, 0, 80).status, "ok"); // no cap -> never warns
        assert_eq!(period_status("week", 850_000, 1_000_000, 0).warn_pct, 80); // default warn
        assert_eq!(
            period_status("month", i64::MAX, i64::MAX, 150).pct,
            100,
            "percentage math must not saturate before division"
        );
        assert_eq!(period_status("month", -1, 1_000, 80).spent_micros, 0);
        assert_eq!(period_status("month", 1, 1_000, 150).warn_pct, 100);
    }

    #[test]
    fn add_cost_saturates_rather_than_overflowing() {
        // The running-spend accumulator is bumped on every captured step; a pathological cumulative
        // total must clamp at i64::MAX rather than panic (debug overflow) or wrap (release), matching
        // the overflow-safe money invariant the rest of the cost path upholds.
        let mut t = RunTally::default();
        t.add_cost(i64::MAX);
        t.add_cost(i64::MAX); // would overflow a plain `+=`
        assert_eq!(t.micros, i64::MAX, "cumulative spend clamps at the ceiling");
        assert_eq!(t.effective_micros(), i64::MAX);
    }

    #[test]
    fn tally_mutations_are_total_at_numeric_boundaries() {
        let mut t = RunTally {
            micros: -10,
            reserved: -10,
            steps: u32::MAX,
            repeats: BTreeMap::from([(7, u32::MAX)]),
            last_decision: None,
        };
        t.note_forwarded(7);
        t.reserve(i64::MAX);
        t.reserve(i64::MAX);
        t.release(i64::MIN);
        t.add_cost(-1);
        assert_eq!(t.steps, u32::MAX);
        assert_eq!(t.repeats[&7], u32::MAX);
        assert_eq!(t.reserved, i64::MAX);
        assert_eq!(t.micros, 0);
        assert_eq!(t.effective_micros(), i64::MAX);

        let b = Budget {
            max_identical_repeats: Some(u32::MAX),
            ..Default::default()
        };
        assert!(matches!(evaluate(&b, &t, 7), Decision::Kill(_)));
    }

    #[test]
    fn invalid_money_lines_are_ignored_and_large_default_warn_is_precise() {
        let invalid = Budget {
            max_micros: Some(-1),
            soft_micros: Some(0),
            ..Default::default()
        };
        assert!(!invalid.is_set());
        assert_eq!(evaluate(&invalid, &RunTally::default(), 1), Decision::Allow);

        let max = i64::MAX;
        let warn_at = (i128::from(max) * 80 / 100) as i64;
        let large = Budget {
            max_micros: Some(max),
            ..Default::default()
        };
        let tally = RunTally {
            micros: warn_at,
            ..Default::default()
        };
        assert!(matches!(evaluate(&large, &tally, 1), Decision::Warn(_)));
    }

    #[test]
    fn step_limit() {
        let b = Budget {
            max_steps: Some(1),
            ..Default::default()
        };
        let mut t = RunTally::default();
        assert_eq!(evaluate(&b, &t, 1), Decision::Allow);
        t.note_forwarded(1);
        assert!(matches!(evaluate(&b, &t, 2), Decision::Kill(_)));
    }
}
