//! Estimate-Confidence: one always-visible signal for how much to trust the dollar
//! figure, fusing three inputs Tare scatters today — pricing FRESHNESS (a >90-day table drifts),
//! UNPRICED token share (models the bundled table can't price contribute tokens but $0), and
//! capture COVERAGE (are we seeing all the spend, or is a channel dark). Pure + deterministic;
//! the caller supplies the three numbers (pricing age vs an injected `today`, the report's
//! unpriced share, and coverage — keeping core clock-free and storage-free).

use serde::{Deserialize, Serialize};

/// Honest capture-coverage state. Out-of-band capture has no defensible
/// denominator for "expected total spend", so coverage is `unknown` by default rather than a
/// fabricated `100%`. `partial` means demonstrated gaps; `full` only with a defensible complete
/// denominator. A numeric percentage (`coverage_share_pct`) is emitted ONLY when such a denominator
/// exists — never invented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Unknown,
    Partial,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstimateConfidence {
    /// Days since the pricing table's effective date (older = more drift).
    pub pricing_age_days: i64,
    /// Share of TOKENS on models the table couldn't price (their cost reads as $0).
    pub unpriced_token_share_pct: i64,
    /// Capture coverage as an honest STATUS, not a fabricated number.
    pub coverage_status: CoverageStatus,
    /// Numeric coverage share — present ONLY when a defensible denominator exists; omitted otherwise
    /// so the wire never carries an invented completeness figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_share_pct: Option<i64>,
    /// `high` | `medium` | `low` — the headline.
    pub label: String,
    pub estimated: bool,
}

/// Fuse the factors into a confidence label. Thresholds are deterministic: `high` needs a fresh
/// table, almost everything priced, AND demonstrable full coverage; `medium` tolerates moderate
/// drift / unpriced / unverifiable coverage; anything worse is `low`. Unknown or partial coverage
/// can never reach `high` (we can't claim "nothing dark" without a denominator), but is not itself
/// catastrophic — only a real numeric coverage below 60% is treated as `low`.
pub fn confidence(
    pricing_age_days: i64,
    unpriced_token_share_pct: i64,
    coverage_status: CoverageStatus,
    coverage_share_pct: Option<i64>,
) -> EstimateConfidence {
    let coverage_full = matches!(coverage_status, CoverageStatus::Full)
        || coverage_share_pct.is_some_and(|p| p >= 90);
    // Only a measured number can be "catastrophically low"; unknown/partial are medium-eligible.
    let coverage_low = coverage_share_pct.is_some_and(|p| p < 60);
    let label = if pricing_age_days <= 90 && unpriced_token_share_pct <= 5 && coverage_full {
        "high"
    } else if pricing_age_days > 180 || unpriced_token_share_pct > 25 || coverage_low {
        "low"
    } else {
        "medium"
    };
    EstimateConfidence {
        pricing_age_days,
        unpriced_token_share_pct,
        coverage_status,
        coverage_share_pct,
        label: label.to_string(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_priced_full_coverage_is_high() {
        // Demonstrable full coverage (status or a >=90 measured share) can reach high.
        assert_eq!(confidence(10, 0, CoverageStatus::Full, None).label, "high");
        assert_eq!(
            confidence(10, 0, CoverageStatus::Unknown, Some(100)).label,
            "high"
        );
    }

    #[test]
    fn moderate_drift_or_unpriced_is_medium() {
        assert_eq!(
            confidence(120, 0, CoverageStatus::Full, None).label,
            "medium"
        ); // stale-ish pricing
        assert_eq!(
            confidence(10, 15, CoverageStatus::Full, None).label,
            "medium"
        ); // some unpriced
    }

    #[test]
    fn unknown_or_partial_coverage_caps_at_medium_never_high() {
        // No denominator → we cannot claim "nothing dark", so the ceiling is medium even when the
        // table is fresh and everything is priced. It is NOT low, though — the captured
        // dollars are still moderately trustworthy.
        assert_eq!(
            confidence(10, 0, CoverageStatus::Unknown, None).label,
            "medium"
        );
        assert_eq!(
            confidence(10, 0, CoverageStatus::Partial, None).label,
            "medium"
        );
        // No fabricated number rides along when there is no denominator.
        assert_eq!(
            confidence(10, 0, CoverageStatus::Unknown, None).coverage_share_pct,
            None
        );
    }

    #[test]
    fn heavy_drift_or_measured_dark_capture_is_low() {
        assert_eq!(confidence(400, 0, CoverageStatus::Full, None).label, "low"); // very stale table
        assert_eq!(confidence(10, 40, CoverageStatus::Full, None).label, "low"); // lots unpriced
        assert_eq!(
            confidence(10, 0, CoverageStatus::Partial, Some(30)).label,
            "low"
        ); // a MEASURED 30% coverage is genuinely low
    }
}
