//! Integer micro-USD money type. No floats anywhere in the cost path.
//!
//! 1 USD = 1_000_000 micro-USD. Rates are stored as micro-USD per **million tokens**,
//! so the cost of `n` tokens at rate `r` is `n * r / 1_000_000`, computed in i128 and
//! truncated toward zero (the single, documented rounding mode).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign};

/// A monetary amount in integer micro-USD (millionths of a dollar).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MicroUsd(pub i64);

impl MicroUsd {
    pub const ZERO: MicroUsd = MicroUsd(0);

    /// Cost of `tokens` at `micro_per_mtok` (micro-USD per 1,000,000 tokens).
    /// Truncates toward zero, then **saturates** to the i64 micro-USD range (no wrap,
    /// no panic) so hostile/corrupt counts can never silently flip the sign.
    pub fn for_tokens(tokens: u64, micro_per_mtok: i64) -> MicroUsd {
        let v = (tokens as i128) * (micro_per_mtok as i128) / 1_000_000i128;
        MicroUsd(v.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
    }

    pub fn micros(self) -> i64 {
        self.0
    }

    /// Render as a fixed 6-decimal dollar string, e.g. `$0.421875`. Deterministic.
    pub fn to_dollar_string(self) -> String {
        let neg = self.0 < 0;
        let abs = self.0.unsigned_abs();
        let dollars = abs / 1_000_000;
        let frac = abs % 1_000_000;
        format!("{}${}.{:06}", if neg { "-" } else { "" }, dollars, frac)
    }

    /// Render as a 2-decimal dollar string, e.g. `$0.42`, rounding to the nearest cent
    /// (half-up on the absolute value). For human-facing narrative; integer-only, deterministic.
    pub fn to_dollar_string_2dp(self) -> String {
        let neg = self.0 < 0;
        let abs = self.0.unsigned_abs();
        // micro -> cents, rounded half-up: (abs + 5000) / 10000.
        let cents = (abs + 5_000) / 10_000;
        format!(
            "{}${}.{:02}",
            if neg { "-" } else { "" },
            cents / 100,
            cents % 100
        )
    }
}

/// Multiply a signed numerator by `scale`, divide by an unsigned count, and saturate the result to
/// the representable i64 range. `None` means the denominator was zero. The i128 intermediate
/// keeps large-but-valid counters from wrapping through a lossy `u64 as i64` conversion.
pub(crate) fn scaled_div(numerator: i64, scale: i64, denominator: u64) -> Option<i64> {
    if denominator == 0 {
        return None;
    }
    let value = i128::from(numerator) * i128::from(scale) / i128::from(denominator);
    Some(value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// Integer percentage of `part` in `whole` (0 when `whole <= 0`), rounded to nearest.
/// No f64 anywhere — for narrative "N% of spend". Deterministic.
pub fn percent_of(part: i64, whole: i64) -> i64 {
    if whole <= 0 {
        return 0;
    }
    let denominator = i128::from(whole);
    let scaled = i128::from(part) * 100;
    let magnitude = (scaled.abs() + denominator / 2) / denominator;
    let rounded = if scaled < 0 { -magnitude } else { magnitude };
    rounded.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

impl Add for MicroUsd {
    type Output = MicroUsd;
    fn add(self, rhs: MicroUsd) -> MicroUsd {
        MicroUsd(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for MicroUsd {
    fn add_assign(&mut self, rhs: MicroUsd) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl std::iter::Sum for MicroUsd {
    fn sum<I: Iterator<Item = MicroUsd>>(iter: I) -> MicroUsd {
        iter.fold(MicroUsd::ZERO, |a, m| a + m)
    }
}

impl fmt::Display for MicroUsd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_dollar_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_toward_zero() {
        // 200 tokens at $5/MTok = 200 * 5_000_000 / 1_000_000 = 1000 micro = $0.001
        assert_eq!(MicroUsd::for_tokens(200, 5_000_000), MicroUsd(1000));
        // 1 token at $5/MTok = 5 micro
        assert_eq!(MicroUsd::for_tokens(1, 5_000_000), MicroUsd(5));
        // 3 tokens at 1 micro/MTok = 0 (truncated)
        assert_eq!(MicroUsd::for_tokens(3, 1), MicroUsd(0));
    }

    #[test]
    fn saturates_at_i64_boundary_no_wrap() {
        // Astronomically large token count at a high rate must saturate, not wrap negative.
        let huge = MicroUsd::for_tokens(u64::MAX, i64::MAX);
        assert_eq!(huge, MicroUsd(i64::MAX));
        assert!(huge.micros() > 0);
        // Saturating add never wraps past i64::MAX.
        assert_eq!(MicroUsd(i64::MAX) + MicroUsd(10), MicroUsd(i64::MAX));
        let s: MicroUsd = [MicroUsd(i64::MAX), MicroUsd(i64::MAX)].into_iter().sum();
        assert_eq!(s, MicroUsd(i64::MAX));
        assert_eq!(scaled_div(i64::MAX, 100, 1), Some(i64::MAX));
        assert_eq!(scaled_div(i64::MIN, 100, 1), Some(i64::MIN));
        assert_eq!(scaled_div(10, 1, u64::MAX), Some(0));
        assert_eq!(scaled_div(10, 1, 0), None);
        assert_eq!(percent_of(i64::MAX, 1), i64::MAX);
        assert_eq!(percent_of(-1, 4), -25);
    }

    #[test]
    fn dollar_string_is_fixed_width() {
        assert_eq!(MicroUsd(421_875).to_dollar_string(), "$0.421875");
        assert_eq!(MicroUsd(25_000_000).to_dollar_string(), "$25.000000");
        assert_eq!(MicroUsd(0).to_dollar_string(), "$0.000000");
    }
}
