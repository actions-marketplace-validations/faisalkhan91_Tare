//! Compare two reports — "did this change make my agent cheaper?". Pure, deterministic.

use crate::attribute::Report;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CauseDelta {
    pub cause: String,
    pub micros_before: i64,
    pub micros_after: i64,
    /// after − before (negative = cheaper).
    pub delta_micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportDiff {
    pub pricing_version: String,
    pub estimated: bool,
    pub total_before: i64,
    pub total_after: i64,
    /// after − before (negative = cheaper overall).
    pub delta_micros: i64,
    pub rows: Vec<CauseDelta>,
}

fn micros_for(report: &Report, cause: &str) -> i64 {
    report
        .rows
        .iter()
        .find(|r| r.cause == cause)
        .map(|r| r.micros)
        .unwrap_or(0)
}

/// Diff `after` against `before`, per cause and overall. Rows ordered by magnitude of
/// change (desc), then cause name (asc) — byte-stable.
pub fn diff_reports(before: &Report, after: &Report) -> ReportDiff {
    let causes: BTreeSet<&str> = before
        .rows
        .iter()
        .chain(after.rows.iter())
        .map(|r| r.cause.as_str())
        .collect();

    let mut rows: Vec<CauseDelta> = causes
        .into_iter()
        .map(|cause| {
            let b = micros_for(before, cause);
            let a = micros_for(after, cause);
            CauseDelta {
                cause: cause.to_string(),
                micros_before: b,
                micros_after: a,
                delta_micros: a.saturating_sub(b),
            }
        })
        .collect();
    rows.sort_by(|x, y| {
        y.delta_micros
            .unsigned_abs()
            .cmp(&x.delta_micros.unsigned_abs())
            .then(x.cause.cmp(&y.cause))
    });

    ReportDiff {
        pricing_version: after.pricing_version.clone(),
        estimated: true,
        total_before: before.total_micros,
        total_after: after.total_micros,
        delta_micros: after.total_micros.saturating_sub(before.total_micros),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribute::TrimRow;

    fn report(total: i64, rows: &[(&str, i64)]) -> Report {
        Report {
            pricing_version: "fixture-2026.06".into(),
            effective_date: "2026-06-01".into(),
            estimated: true,
            total_micros: total,
            rows: rows
                .iter()
                .map(|(c, m)| TrimRow {
                    cause: c.to_string(),
                    detail: String::new(),
                    tokens: 0,
                    micros: *m,
                    projected_saved_micros: 0,
                })
                .collect(),
            unpriced: Vec::new(),
            privacy_policy_id: None,
            profile: None,
            attribution_confidence: None,
        }
    }

    #[test]
    fn diffs_total_and_per_cause() {
        let before = report(
            100_000,
            &[("bloated-system-prompt", 80_000), ("retry-loop", 20_000)],
        );
        // caching the system prompt away, but a new verbose-tool cause appears.
        let after = report(
            40_000,
            &[("retry-loop", 20_000), ("verbose-tool-output", 20_000)],
        );
        let d = diff_reports(&before, &after);
        assert_eq!(d.total_before, 100_000);
        assert_eq!(d.total_after, 40_000);
        assert_eq!(d.delta_micros, -60_000);
        // Largest-magnitude change first: bloated dropped 80k.
        assert_eq!(d.rows[0].cause, "bloated-system-prompt");
        assert_eq!(d.rows[0].delta_micros, -80_000);
        // retry unchanged -> delta 0, ordered last.
        let retry = d.rows.iter().find(|r| r.cause == "retry-loop").unwrap();
        assert_eq!(retry.delta_micros, 0);
    }

    #[test]
    fn extreme_deltas_saturate_and_sort_without_panicking() {
        let before = report(i64::MAX, &[("x", i64::MAX)]);
        let after = report(i64::MIN, &[("x", i64::MIN)]);
        let d = diff_reports(&before, &after);
        assert_eq!(d.delta_micros, i64::MIN);
        assert_eq!(d.rows[0].delta_micros, i64::MIN);
    }
}
