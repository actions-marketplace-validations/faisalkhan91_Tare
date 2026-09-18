//! Cost-regression bisection over a dense daily spend series: the first day whose spend crosses
//! a threshold above the trailing median. Pure, integer (no f64 — the ratio test is an i128
//! cross-multiplication), deterministic, clock-free (dates are passed in via the series).

use serde::{Deserialize, Serialize};

use crate::money::scaled_div;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Regression {
    pub date: String,
    pub value_micros: i64,
    pub baseline_micros: i64,
    /// How far over baseline, in percent (integer, floor).
    pub pct_over: i64,
}

/// Integer median of a window (sorted copy). Even length -> floor of the two middle values.
pub fn median(values: &[i64]) -> i64 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        // i128 to avoid overflow on the sum before halving.
        (((v[n / 2 - 1] as i128) + (v[n / 2] as i128)) / 2) as i64
    }
}

/// Median of the non-zero days in a window. A dense trend series carries an explicit
/// `0` on every idle/gap day, so a plain windowed median collapses to 0 for an intermittent user
/// (the documented target) and the `baseline <= 0` guard would suppress EVERY spike/regression.
/// Basing the baseline on active days reflects typical spend on the days the user actually works;
/// an all-idle window still yields 0 (genuinely nothing to regress against → correctly skipped).
pub fn active_median(values: &[i64]) -> i64 {
    let active: Vec<i64> = values.iter().copied().filter(|&x| x > 0).collect();
    median(&active)
}

/// First index `i` (>= 1) whose `values[i]` exceeds the trailing-median baseline of the prior
/// `window` days by more than `threshold_pct` percent. `days[i]`/`values[i]` are aligned and
/// dense. Returns `None` if nothing crosses (e.g. flat or declining spend). A zero/empty
/// baseline is skipped (can't regress against nothing).
pub fn bisect(
    days: &[String],
    values: &[i64],
    window: usize,
    threshold_pct: i64,
) -> Option<Regression> {
    let n = days.len().min(values.len());
    let window = window.max(1);
    let threshold_pct = threshold_pct.max(0);
    for i in 1..n {
        let start = i.saturating_sub(window);
        let baseline = active_median(&values[start..i]); // Ignore idle days in the baseline.
        if baseline <= 0 {
            continue;
        }
        let value = values[i];
        // value > baseline * (100 + threshold_pct) / 100, as integer cross-multiplication.
        let lhs = (value as i128) * 100;
        let rhs = (baseline as i128) * (100 + threshold_pct as i128);
        if lhs > rhs {
            let pct_over =
                scaled_div(value.saturating_sub(baseline), 100, baseline as u64).unwrap_or(0);
            return Some(Regression {
                date: days[i].clone(),
                value_micros: value,
                baseline_micros: baseline,
                pct_over,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("2026-06-{:02}", i + 1)).collect()
    }

    #[test]
    fn finds_first_spike_over_threshold() {
        // Flat ~100, then a 3x spike on day 5.
        let values = vec![100, 110, 90, 100, 320, 330];
        let r = bisect(&days(6), &values, 4, 50).unwrap();
        assert_eq!(r.date, "2026-06-05");
        assert_eq!(r.value_micros, 320);
        assert!(r.pct_over >= 200); // ~3x over a ~100 baseline
    }

    #[test]
    fn no_regression_on_flat_or_declining() {
        assert!(bisect(&days(5), &[100, 100, 100, 100, 100], 3, 50).is_none());
        assert!(bisect(&days(4), &[400, 300, 200, 100], 3, 50).is_none());
    }

    #[test]
    fn zero_baseline_is_skipped_not_a_regression() {
        // First real spend after zero days is not a "regression" (nothing to compare to).
        let r = bisect(&days(3), &[0, 0, 500], 3, 50);
        assert!(r.is_none());
    }

    #[test]
    fn intermittent_user_regression_survives_idle_days() {
        // A plain windowed median over an intermittent series is 0 because idle days
        // dominate), which used to suppress detection. The active-day baseline (~100) catches the
        // 5x jump on the last active day.
        let values = vec![100, 0, 0, 100, 0, 0, 100, 500];
        let r = bisect(&days(8), &values, 7, 50).expect("5x jump over the active baseline");
        assert_eq!(r.value_micros, 500);
        assert_eq!(r.baseline_micros, 100);
        // An all-idle prior window still has no baseline → correctly not a regression.
        assert!(bisect(&days(4), &[0, 0, 0, 500], 3, 50).is_none());
    }

    #[test]
    fn threshold_is_integer_cross_multiplication() {
        // baseline 100, value 149 with threshold 50% -> not over (149 <= 150). 151 -> over.
        assert!(bisect(&days(2), &[100, 149], 1, 50).is_none());
        assert!(bisect(&days(2), &[100, 151], 1, 50).is_some());
        assert!(bisect(&days(2), &[100, 100], 1, -50).is_none());
    }
}
