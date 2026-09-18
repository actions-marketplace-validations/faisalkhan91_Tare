//! Weekly digest: the habit-loop surface. Recomposes the shipped primitives — trend
//! (week-over-week delta + top drivers), anomaly detection (what's new this week), and the savings
//! ledger (unclaimed dollars) — into one glanceable local report. ZERO new capture; a pure
//! re-projection of stored counts. Rendered to a file / desktop notification, **never emailed or
//! sent off-box** (the CLI owns delivery; this module only computes + renders text).
//!
//! Clock-free: the caller passes the reference date (`today`), and the two 7-day windows are
//! `this_week = [today-6, today]` and `last_week = [today-13, today-7]`. Pure, integer.

use crate::anomaly::{detect, Anomaly};
use crate::calendar::{format_date, parse_date};
use crate::money::scaled_div;
use crate::money::MicroUsd;
use crate::pricing::PricingTable;
use crate::savings::savings;
use crate::trend::{trend, DatedRun, TrendDimension};
use serde::{Deserialize, Serialize};

/// One top spend driver (a model) in the digest week.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestDriver {
    pub label: String,
    pub micros: i64,
}

/// A week's glanceable cost digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Digest {
    pub week_start: String,
    pub week_end: String,
    pub this_week_micros: i64,
    pub last_week_micros: i64,
    /// this_week − last_week (signed; negative = spent less than the prior week).
    pub wow_delta_micros: i64,
    /// Percent change vs last week (0 when last week had no spend, to avoid a divide-by-zero blowup).
    pub wow_delta_pct: i64,
    /// Top models by spend this week (largest first), capped.
    pub top_drivers: Vec<DigestDriver>,
    /// Anomalies whose day falls in this week (materiality-tiered by the detector).
    pub new_anomalies: Vec<Anomaly>,
    /// Spend-bounded capped potential across this week's runs; opportunity categories can overlap.
    pub unclaimed_savings_micros: i64,
    pub pricing_version: String,
    pub estimated: bool,
}

/// How many top drivers to include.
const TOP_DRIVERS: usize = 5;
/// Anomaly detection window + threshold, matching `tare trend --anomalies` defaults.
const ANOMALY_WINDOW: usize = 7;
const ANOMALY_THRESHOLD_PCT: i64 = 50;

/// Build the weekly digest ending on `today` (YYYY-MM-DD). Falls back to an all-zero digest with a
/// well-formed window if `today` doesn't parse (never panics on a bad date).
pub fn digest(dated: &[DatedRun], pricing: &PricingTable, today: &str) -> Digest {
    let Some(today_days) = parse_date(today) else {
        return Digest {
            week_start: today.to_string(),
            week_end: today.to_string(),
            this_week_micros: 0,
            last_week_micros: 0,
            wow_delta_micros: 0,
            wow_delta_pct: 0,
            top_drivers: vec![],
            new_anomalies: vec![],
            unclaimed_savings_micros: 0,
            pricing_version: pricing.version.clone(),
            estimated: true,
        };
    };
    let this_start = format_date(today_days - 6);
    let last_start = format_date(today_days - 13);

    // One 14-day Total trend spans both weeks; split its dense per-day axis at the midpoint.
    let total = trend(dated, &last_start, today, pricing, TrendDimension::Total);
    let (mut this_week_micros, mut last_week_micros) = (0i64, 0i64);
    for (i, day) in total.days.iter().enumerate() {
        let day_micros = total
            .series
            .iter()
            .map(|s| s.per_day.get(i).copied().unwrap_or(0))
            .fold(0i64, i64::saturating_add);
        if day.as_str() >= this_start.as_str() {
            this_week_micros = this_week_micros.saturating_add(day_micros);
        } else {
            last_week_micros = last_week_micros.saturating_add(day_micros);
        }
    }
    let wow_delta_micros = this_week_micros.saturating_sub(last_week_micros);
    let wow_delta_pct = if last_week_micros > 0 {
        scaled_div(wow_delta_micros, 100, last_week_micros as u64).unwrap_or(0)
    } else {
        0
    };

    // Top drivers: this week's spend broken down by model.
    let by_model = trend(dated, &this_start, today, pricing, TrendDimension::ByModel);
    let mut top_drivers: Vec<DigestDriver> = by_model
        .series
        .iter()
        .filter(|s| s.total_micros > 0)
        .map(|s| DigestDriver {
            label: s.key.clone(),
            micros: s.total_micros,
        })
        .collect();
    // `trend` already sorts series by total desc; cap to the top N.
    top_drivers.truncate(TOP_DRIVERS);

    // New anomalies: detect over the two-week Total trend, keep those landing in this week.
    let new_anomalies: Vec<Anomaly> = detect(&total, ANOMALY_WINDOW, ANOMALY_THRESHOLD_PCT)
        .into_iter()
        .filter(|a| a.date.as_str() >= this_start.as_str())
        .collect();

    // Unclaimed capped potential over this week's runs (bounded by spend, not overlap-subtracted).
    let this_week_runs: Vec<crate::model::RunRecord> = dated
        .iter()
        .filter(|dr| dr.date.as_str() >= this_start.as_str() && dr.date.as_str() <= today)
        .map(|dr| dr.run.clone())
        .collect();
    let unclaimed_savings_micros = savings(&this_week_runs, pricing).total_recoverable_micros;

    Digest {
        week_start: this_start,
        week_end: today.to_string(),
        this_week_micros,
        last_week_micros,
        wow_delta_micros,
        wow_delta_pct,
        top_drivers,
        new_anomalies,
        unclaimed_savings_micros,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

/// Render the digest as a compact, plain-text report (what gets written to a file / notification).
pub fn render_digest_text(d: &Digest) -> String {
    let mut out = format!(
        "Tare weekly digest — {} to {} (ESTIMATE, local; pricing {})\n\n",
        d.week_start, d.week_end, d.pricing_version
    );
    let arrow = if d.wow_delta_micros > 0 {
        "▲"
    } else if d.wow_delta_micros < 0 {
        "▼"
    } else {
        "—"
    };
    out.push_str(&format!(
        "Spend: {}  ({arrow} {} vs last week, {}{}%)\n",
        MicroUsd(d.this_week_micros).to_dollar_string(),
        MicroUsd(d.wow_delta_micros.saturating_abs()).to_dollar_string(),
        if d.wow_delta_pct >= 0 { "+" } else { "" },
        d.wow_delta_pct,
    ));
    out.push_str(&format!(
        "Last week: {}\n\n",
        MicroUsd(d.last_week_micros).to_dollar_string()
    ));

    out.push_str("Top drivers this week:\n");
    if d.top_drivers.is_empty() {
        out.push_str("  (no spend)\n");
    } else {
        for dr in &d.top_drivers {
            out.push_str(&format!(
                "  {:<28} {}\n",
                dr.label,
                MicroUsd(dr.micros).to_dollar_string()
            ));
        }
    }

    out.push_str(&format!("\nNew anomalies: {}\n", d.new_anomalies.len()));
    for a in &d.new_anomalies {
        out.push_str(&format!(
            "  {} {} [{}] {} (baseline {})\n",
            a.date,
            a.series_key,
            a.materiality,
            MicroUsd(a.value_micros).to_dollar_string(),
            MicroUsd(a.baseline_micros).to_dollar_string(),
        ));
    }

    out.push_str(&format!(
        "\nUnclaimed savings: {}  (see `tare` Optimize / savings)\n",
        MicroUsd(d.unclaimed_savings_micros).to_dollar_string()
    ));
    out
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

    #[test]
    fn splits_weeks_and_computes_wow_delta() {
        // last week: 2026-06-15 spent 1M input ($3). this week: 2026-06-24 spent 2M ($6).
        let runs = vec![
            dated("2026-06-15", 1_000_000),
            dated("2026-06-24", 2_000_000),
        ];
        let d = digest(&runs, &pricing(), "2026-06-27");
        assert_eq!(d.week_start, "2026-06-21");
        assert_eq!(d.week_end, "2026-06-27");
        assert_eq!(
            d.last_week_micros, 3_000_000,
            "2026-06-15 is in [06-14, 06-20]"
        );
        assert_eq!(
            d.this_week_micros, 6_000_000,
            "2026-06-24 is in [06-21, 06-27]"
        );
        assert_eq!(d.wow_delta_micros, 3_000_000);
        assert_eq!(d.wow_delta_pct, 100, "doubled week-over-week");
        assert_eq!(d.top_drivers.len(), 1);
        assert_eq!(d.top_drivers[0].label, "m");
        assert_eq!(d.top_drivers[0].micros, 6_000_000);
    }

    #[test]
    fn empty_when_no_spend_in_window() {
        let d = digest(&[], &pricing(), "2026-06-27");
        assert_eq!(d.this_week_micros, 0);
        assert_eq!(
            d.wow_delta_pct, 0,
            "no divide-by-zero on an empty prior week"
        );
        assert!(d.top_drivers.is_empty());
        assert!(d.new_anomalies.is_empty());
        assert!(render_digest_text(&d).contains("weekly digest"));
    }

    #[test]
    fn bad_date_degrades_gracefully() {
        let d = digest(&[dated("2026-06-24", 1_000_000)], &pricing(), "not-a-date");
        assert_eq!(d.this_week_micros, 0);
        assert!(d.estimated);
    }

    #[test]
    fn extreme_weekly_values_saturate_without_distorting_percent() {
        let computed = digest(&[dated("2026-06-15", u64::MAX)], &pricing(), "2026-06-27");
        assert_eq!(computed.last_week_micros, i64::MAX);
        assert_eq!(computed.wow_delta_micros, -i64::MAX);
        assert_eq!(computed.wow_delta_pct, -100);

        let d = Digest {
            week_start: "2026-06-21".into(),
            week_end: "2026-06-27".into(),
            this_week_micros: 0,
            last_week_micros: i64::MAX,
            wow_delta_micros: i64::MIN,
            wow_delta_pct: -100,
            top_drivers: vec![],
            new_anomalies: vec![],
            unclaimed_savings_micros: 0,
            pricing_version: "v".into(),
            estimated: true,
        };
        let text = render_digest_text(&d);
        assert!(text.contains("$9223372036854.775807"));
    }
}
