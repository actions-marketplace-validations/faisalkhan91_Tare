//! Savings-realization lifecycle: proving whether accepted advice actually paid off.
//! When the user acts on a savings opportunity they mark it accepted (the store snapshots the accept
//! date and the recoverable estimate). This module compares TOTAL spend in the equal-length windows
//! before vs after that date (before = the W days ending the day before accept; after = the W days
//! from accept) and reports the lifecycle: open then accepted then realized.
//!
//! Honest by construction: realized savings is `max(0, before − after)` (spend genuinely dropped),
//! and stays `pending` until the after-window has fully elapsed by `today` — so a fresh acceptance
//! never claims a win it can't yet prove. Clock-free (the caller passes `today`); pure, integer.
//! Reuses the tested `trend` engine for time-effective spend, so a price change reprices history.

use crate::calendar::{format_date, parse_date};
use crate::pricing::PricingTable;
use crate::trend::{trend, DatedRun, TrendDimension};
use serde::{Deserialize, Serialize};

/// A user's acceptance of a savings opportunity (mirrors the store's row; core-side input so
/// tare-core needn't depend on tare-store).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedOpportunity {
    pub opportunity_key: String,
    pub accepted_date: String,
    pub recoverable_micros: i64,
}

/// One opportunity's realization outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizedRow {
    pub opportunity_key: String,
    pub accepted_date: String,
    /// The recoverable estimate snapshotted at accept time.
    pub recoverable_micros: i64,
    /// Total spend in the W days before the accept date.
    pub before_micros: i64,
    /// Total spend in the W days from the accept date (capped at `today`).
    pub after_micros: i64,
    /// `max(0, before − after)` — the proven drop in spend across the window.
    pub realized_micros: i64,
    pub window_days: i64,
    /// True once the after-window has fully elapsed by `today` (otherwise the verdict is pending).
    pub complete: bool,
    /// `pending` (after-window not yet elapsed) | `realized` (spend dropped) | `not-realized`.
    pub state: String,
}

/// The realization ledger across all accepted opportunities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationLedger {
    /// Accepted opportunities, most-realized first (ties by key).
    pub rows: Vec<RealizedRow>,
    /// Sum of `realized_micros` over rows whose window is complete.
    pub total_realized_micros: i64,
    pub window_days: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Total spend (micro-USD) over `[from, to]`, priced time-effectively (reuses the trend engine).
fn spend_in(dated: &[DatedRun], from: &str, to: &str, pricing: &PricingTable) -> i64 {
    trend(dated, from, to, pricing, TrendDimension::Total)
        .series
        .iter()
        .flat_map(|s| s.per_day.iter())
        .fold(0i64, |a, m| a.saturating_add(*m))
}

/// Build the realization ledger. `window_days` (W) sets the comparison window length on each side.
pub fn realization(
    accepted: &[AcceptedOpportunity],
    dated: &[DatedRun],
    pricing: &PricingTable,
    today: &str,
    window_days: i64,
) -> RealizationLedger {
    let w = window_days.max(1);
    let today_days = parse_date(today);
    let mut rows: Vec<RealizedRow> = Vec::new();
    for a in accepted {
        let Some(acc_days) = parse_date(&a.accepted_date) else {
            continue; // skip an unparseable stored date rather than panic
        };
        let before_from = format_date(acc_days.saturating_sub(w));
        let before_to = format_date(acc_days.saturating_sub(1));
        let after_from = a.accepted_date.clone();
        let after_end_days = acc_days.saturating_add(w).saturating_sub(1);
        // Cap the after-window at today; it's only complete once today has reached its end.
        let (after_to, complete) = match today_days {
            Some(t) if t < after_end_days => (format_date(t), false),
            _ => (format_date(after_end_days), true),
        };

        let before_micros = spend_in(dated, &before_from, &before_to, pricing);
        let after_micros = spend_in(dated, &after_from, &after_to, pricing);
        let realized_micros = before_micros.saturating_sub(after_micros).max(0);
        let state = if !complete {
            "pending"
        } else if realized_micros > 0 {
            "realized"
        } else {
            "not-realized"
        };
        rows.push(RealizedRow {
            opportunity_key: a.opportunity_key.clone(),
            accepted_date: a.accepted_date.clone(),
            recoverable_micros: a.recoverable_micros,
            before_micros,
            after_micros,
            realized_micros,
            window_days: w,
            complete,
            state: state.to_string(),
        });
    }
    rows.sort_by(|a, b| {
        b.realized_micros
            .cmp(&a.realized_micros)
            .then(a.opportunity_key.cmp(&b.opportunity_key))
    });
    let total_realized_micros = rows
        .iter()
        .filter(|r| r.complete)
        .fold(0i64, |a, r| a.saturating_add(r.realized_micros));
    RealizationLedger {
        rows,
        total_realized_micros,
        window_days: w,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Provider, RequestShape, RunRecord, StepRecord, UsageTokens};

    fn pricing() -> PricingTable {
        PricingTable::from_json_str(
            r#"{"version":"t","effective_date":"2026-06-01","model":[
              {"provider":"anthropic","model_id":"m","input_micro_per_mtok":3000000,
               "output_micro_per_mtok":15000000,"cache_read_micro_per_mtok":300000,
               "cache_write_5m_micro_per_mtok":3750000,"cache_write_1h_micro_per_mtok":6000000}]}"#,
        )
        .unwrap()
    }

    fn dated(date: &str, input: u64) -> DatedRun {
        let step = StepRecord {
            run_id: date.into(),
            step_ordinal: 1,
            provider: Provider::Anthropic,
            model: "m".into(),
            usage: UsageTokens {
                fresh_input: input,
                ..Default::default()
            },
            shape: RequestShape {
                model: "m".into(),
                provider: Provider::Anthropic,
                stream: false,
                ttl: crate::model::CacheTtl::FiveMin,
                has_cache_control: false,
                cached_component: None,
                system_hash: None,
                weights: vec![],
                request_hash: Some(0),
                step_label: None,
                component_label: None,
                parent_label: None,
                attempt: None,
                session: None,
                workload_key: None,
                effort: None,
                mcp_server: None,
                vendor: None,
                commit: None,
                author: None,
            },
            stop_reason: None,
            duration_ms: 0,
            start_unix_nano: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        };
        DatedRun {
            date: date.into(),
            run: RunRecord {
                run_id: date.into(),
                steps: vec![step],
            },
        }
    }

    fn accepted(date: &str) -> AcceptedOpportunity {
        AcceptedOpportunity {
            opportunity_key: "loop:search".into(),
            accepted_date: date.into(),
            recoverable_micros: 3_000_000,
        }
    }

    #[test]
    fn realized_when_spend_drops_after_accept() {
        // W=3. Before [06-11,06-13] spends 2M input ($6). After [06-14,06-16] spends 1M ($3).
        let runs = vec![
            dated("2026-06-12", 2_000_000),
            dated("2026-06-15", 1_000_000),
        ];
        // accepted 06-14; today 06-20 (after-window fully elapsed).
        let led = realization(
            &[accepted("2026-06-14")],
            &runs,
            &pricing(),
            "2026-06-20",
            3,
        );
        let r = &led.rows[0];
        assert!(r.complete);
        assert_eq!(r.before_micros, 6_000_000);
        assert_eq!(r.after_micros, 3_000_000);
        assert_eq!(r.realized_micros, 3_000_000, "spend halved → $3 realized");
        assert_eq!(r.state, "realized");
        assert_eq!(led.total_realized_micros, 3_000_000);
    }

    #[test]
    fn pending_until_after_window_elapses() {
        let runs = vec![dated("2026-06-12", 2_000_000)];
        // accepted 06-14, W=7 → after-window ends 06-20; today 06-16 is short of that.
        let led = realization(
            &[accepted("2026-06-14")],
            &runs,
            &pricing(),
            "2026-06-16",
            7,
        );
        assert_eq!(led.rows[0].state, "pending");
        assert!(!led.rows[0].complete);
        assert_eq!(led.total_realized_micros, 0, "pending rows don't count yet");
    }

    #[test]
    fn not_realized_when_spend_did_not_drop() {
        // Same spend before and after → no realized savings.
        let runs = vec![
            dated("2026-06-12", 1_000_000),
            dated("2026-06-15", 1_000_000),
        ];
        let led = realization(
            &[accepted("2026-06-14")],
            &runs,
            &pricing(),
            "2026-06-20",
            3,
        );
        assert_eq!(led.rows[0].state, "not-realized");
        assert_eq!(led.rows[0].realized_micros, 0);
    }
}
