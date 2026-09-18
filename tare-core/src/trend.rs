//! Spend trends over a dense calendar window. Pure function of dated runs + pricing; no clock
//! (dates arrive as `YYYY-MM-DD` from the store), no `f64`, deterministic ordering. Every
//! dollar figure is ESTIMATED at view time — no per-day cost is ever persisted.

use crate::attribute::{build_report_dated, step_micros};
use crate::calendar::days_between;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A run tagged with the civil date it was recorded on (from the store's `created_date`).
#[derive(Clone, Debug)]
pub struct DatedRun {
    pub date: String,
    pub run: RunRecord,
}

/// Which axis to break spend down by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendDimension {
    Total,
    ByProvider,
    ByModel,
    ByCause,
}

impl TrendDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            TrendDimension::Total => "total",
            TrendDimension::ByProvider => "by_provider",
            TrendDimension::ByModel => "by_model",
            TrendDimension::ByCause => "by_cause",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "total" => Some(TrendDimension::Total),
            "provider" | "by_provider" => Some(TrendDimension::ByProvider),
            "model" | "by_model" => Some(TrendDimension::ByModel),
            "cause" | "by_cause" => Some(TrendDimension::ByCause),
            _ => None,
        }
    }
}

/// One series (a single bucket of the dimension) with a value per calendar day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrendSeries {
    pub key: String,
    /// Micro-USD per day, aligned 1:1 with `TrendReport.days`. Gap days are an explicit 0.
    pub per_day: Vec<i64>,
    pub total_micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrendReport {
    pub dimension: String,
    pub from: String,
    pub to: String,
    /// Dense calendar axis (every day in [from, to], no gaps).
    pub days: Vec<String>,
    pub series: Vec<TrendSeries>,
    pub pricing_version: String,
    pub estimated: bool,
}

/// Estimated cost (micro-USD) of one run under `pricing`, priced at the rates in effect on `date`
/// (time-effective, #9) — so re-running the trend after a price change reprices history correctly.
fn run_micros(run: &RunRecord, pricing: &PricingTable, date: &str) -> i64 {
    run.steps
        .iter()
        .map(|step| step_micros(step, pricing, Some(date)))
        .fold(0i64, i64::saturating_add)
}

/// Build a dense trend over `[from, to]` broken down by `dim`. Series are sorted by
/// `total_micros` desc, then key asc (deterministic). Gap days are an explicit 0.
pub fn trend(
    dated: &[DatedRun],
    from: &str,
    to: &str,
    pricing: &PricingTable,
    dim: TrendDimension,
) -> TrendReport {
    let days = days_between(from, to);
    let day_index: BTreeMap<&str, usize> = days
        .iter()
        .enumerate()
        .map(|(i, d)| (d.as_str(), i))
        .collect();
    let n = days.len();

    // bucket key -> per-day micros vector.
    let mut buckets: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut add = |key: String, day_pos: usize, micros: i64| {
        let v = buckets.entry(key).or_insert_with(|| vec![0i64; n]);
        v[day_pos] = v[day_pos].saturating_add(micros);
    };

    for dr in dated {
        let Some(&pos) = day_index.get(dr.date.as_str()) else {
            continue; // outside the window
        };
        match dim {
            TrendDimension::Total => add(
                "total".to_string(),
                pos,
                run_micros(&dr.run, pricing, &dr.date),
            ),
            TrendDimension::ByProvider => {
                // Sum per provider across the run's steps (priced at the run's date, #9).
                let mut per: BTreeMap<&'static str, i64> = BTreeMap::new();
                for s in &dr.run.steps {
                    let micros = step_micros(s, pricing, Some(&dr.date));
                    if micros > 0 {
                        let entry = per.entry(s.provider.as_str()).or_insert(0);
                        *entry = entry.saturating_add(micros);
                    }
                }
                for (k, m) in per {
                    add(k.to_string(), pos, m);
                }
            }
            TrendDimension::ByModel => {
                let mut per: BTreeMap<String, i64> = BTreeMap::new();
                for s in &dr.run.steps {
                    let micros = step_micros(s, pricing, Some(&dr.date));
                    if micros > 0 {
                        let entry = per.entry(s.model.clone()).or_insert(0);
                        *entry = entry.saturating_add(micros);
                    }
                }
                for (k, m) in per {
                    add(k, pos, m);
                }
            }
            TrendDimension::ByCause => {
                // Per-day attribution keeps the sum of rows at or below that day's total.
                let day_by_run = BTreeMap::from([(dr.run.run_id.clone(), dr.date.clone())]);
                let report =
                    build_report_dated(std::slice::from_ref(&dr.run), pricing, &day_by_run);
                for row in report
                    .rows
                    .into_iter()
                    // This report-only fallback overlaps the priced step total by design; it is
                    // evidence-quality metadata, not a disjoint spend cause for a stacked trend.
                    .filter(|row| row.cause != "coarse-attribution")
                {
                    add(row.cause, pos, row.micros);
                }
            }
        }
    }

    let mut series: Vec<TrendSeries> = buckets
        .into_iter()
        .map(|(key, per_day)| {
            let total_micros = per_day.iter().fold(0i64, |a, m| a.saturating_add(*m));
            TrendSeries {
                key,
                per_day,
                total_micros,
            }
        })
        .collect();
    // Sort: largest total first, ties broken by key for determinism.
    series.sort_by(|a, b| b.total_micros.cmp(&a.total_micros).then(a.key.cmp(&b.key)));

    TrendReport {
        dimension: dim.as_str().to_string(),
        from: from.to_string(),
        to: to.to_string(),
        days,
        series,
        pricing_version: pricing.version.clone(),
        estimated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest_step;
    use crate::model::Provider;

    fn pricing() -> PricingTable {
        PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml")).unwrap()
    }
    fn dated(date: &str, run_id: &str, provider: Provider, req: &[u8], resp: &[u8]) -> DatedRun {
        let step = ingest_step(run_id, 1, provider, req, resp).unwrap();
        DatedRun {
            date: date.to_string(),
            run: RunRecord {
                run_id: run_id.to_string(),
                steps: vec![step],
            },
        }
    }

    #[test]
    fn dense_axis_with_gap_day_zero_and_window_total() {
        let req = include_bytes!("../../fixtures/openai_nonstream/request.json");
        let resp = include_bytes!("../../fixtures/openai_nonstream/response.json");
        let runs = vec![
            dated("2026-06-20", "a", Provider::Openai, req, resp),
            dated("2026-06-22", "b", Provider::Openai, req, resp),
        ];
        let p = pricing();
        let t = trend(&runs, "2026-06-20", "2026-06-23", &p, TrendDimension::Total);
        // Dense 4-day axis.
        assert_eq!(t.days.len(), 4);
        let s = &t.series[0];
        assert_eq!(s.per_day.len(), 4);
        // Day 20 and 22 have spend; 21 and 23 are explicit 0.
        assert!(s.per_day[0] > 0 && s.per_day[2] > 0);
        assert_eq!(s.per_day[1], 0);
        assert_eq!(s.per_day[3], 0);
        // Window total = sum of per-day.
        assert_eq!(s.total_micros, s.per_day.iter().sum::<i64>());
    }

    #[test]
    fn per_day_cause_series_within_total() {
        // Per-day cause-series sums stay at or below that day's total.
        let req = include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json");
        let resp = include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json");
        let runs = vec![dated("2026-06-20", "a", Provider::Anthropic, req, resp)];
        let p = pricing();
        let total = trend(&runs, "2026-06-20", "2026-06-20", &p, TrendDimension::Total).series[0]
            .per_day[0];
        let cause = trend(
            &runs,
            "2026-06-20",
            "2026-06-20",
            &p,
            TrendDimension::ByCause,
        );
        let cause_sum: i64 = cause.series.iter().map(|s| s.per_day[0]).sum();
        assert!(
            cause_sum <= total,
            "Σ causes {cause_sum} must be ≤ total {total}"
        );
    }

    #[test]
    fn empty_window_yields_no_days() {
        let p = pricing();
        let t = trend(&[], "2026-06-25", "2026-06-24", &p, TrendDimension::Total);
        assert!(t.days.is_empty() && t.series.is_empty());
    }

    #[test]
    fn cause_trend_uses_each_runs_effective_pricing_edition() {
        let make_run = |date: &str, run_id: &str| {
            let steps = vec![
                ingest_step(
                    run_id,
                    1,
                    Provider::Anthropic,
                    include_bytes!("../../fixtures/bloated_system_prompt/step1.request.json"),
                    include_bytes!("../../fixtures/bloated_system_prompt/step1.response.json"),
                )
                .unwrap(),
                ingest_step(
                    run_id,
                    2,
                    Provider::Anthropic,
                    include_bytes!("../../fixtures/bloated_system_prompt/step2.request.json"),
                    include_bytes!("../../fixtures/bloated_system_prompt/step2.response.json"),
                )
                .unwrap(),
            ];
            DatedRun {
                date: date.to_string(),
                run: RunRecord {
                    run_id: run_id.to_string(),
                    steps,
                },
            }
        };
        let pricing = PricingTable::from_toml_str(
            r#"
version = "dated"
effective_date = "2026-01-01"
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
input_micro_per_mtok = 1000000
output_micro_per_mtok = 2000000
cache_read_micro_per_mtok = 100000
cache_write_5m_micro_per_mtok = 1250000
cache_write_1h_micro_per_mtok = 2000000
[[model]]
provider = "anthropic"
model_id = "claude-opus-4-8"
effective_date = "2026-07-01"
input_micro_per_mtok = 2000000
output_micro_per_mtok = 4000000
cache_read_micro_per_mtok = 200000
cache_write_5m_micro_per_mtok = 2500000
cache_write_1h_micro_per_mtok = 4000000
"#,
        )
        .unwrap();
        let runs = vec![make_run("2026-06-15", "old"), make_run("2026-07-15", "new")];

        let total = trend(
            &runs,
            "2026-06-15",
            "2026-07-15",
            &pricing,
            TrendDimension::Total,
        );
        let causes = trend(
            &runs,
            "2026-06-15",
            "2026-07-15",
            &pricing,
            TrendDimension::ByCause,
        );
        let old = 0;
        let new = causes.days.len() - 1;
        let cause_old = causes
            .series
            .iter()
            .fold(0i64, |sum, s| sum.saturating_add(s.per_day[old]));
        let cause_new = causes
            .series
            .iter()
            .fold(0i64, |sum, s| sum.saturating_add(s.per_day[new]));
        assert!(cause_old > 0, "fixture must produce an attributed cause");
        assert_eq!(cause_new, cause_old.saturating_mul(2));
        assert!(cause_old <= total.series[0].per_day[old]);
        assert!(cause_new <= total.series[0].per_day[new]);
    }

    #[test]
    fn cause_trend_excludes_overlapping_coarse_attribution_metadata() {
        let mut run = dated(
            "2026-06-20",
            "coarse",
            Provider::Openai,
            include_bytes!("../../fixtures/openai_nonstream/request.json"),
            include_bytes!("../../fixtures/openai_nonstream/response.json"),
        );
        run.run.steps[0].shape.weights.clear();
        run.run.steps[0].shape.system_hash = None;
        let causes = trend(
            &[run],
            "2026-06-20",
            "2026-06-20",
            &pricing(),
            TrendDimension::ByCause,
        );
        assert!(causes
            .series
            .iter()
            .all(|series| series.key != "coarse-attribution"));
    }

    #[test]
    fn aggregate_series_saturate_on_hostile_usage_counts() {
        let mut run = dated(
            "2026-06-20",
            "huge",
            Provider::Openai,
            include_bytes!("../../fixtures/openai_nonstream/request.json"),
            include_bytes!("../../fixtures/openai_nonstream/response.json"),
        );
        run.run.steps[0].usage.fresh_input = u64::MAX;
        run.run.steps[0].usage.output = 0;
        let mut second = run.run.steps[0].clone();
        second.step_ordinal = 2;
        let mut third = run.run.steps[0].clone();
        third.step_ordinal = 3;
        run.run.steps.extend([second, third]);

        for dimension in [
            TrendDimension::Total,
            TrendDimension::ByProvider,
            TrendDimension::ByModel,
        ] {
            let report = trend(
                std::slice::from_ref(&run),
                "2026-06-20",
                "2026-06-20",
                &pricing(),
                dimension,
            );
            assert_eq!(report.series[0].total_micros, i64::MAX);
            assert_eq!(report.series[0].per_day, vec![i64::MAX]);
        }
    }
}
