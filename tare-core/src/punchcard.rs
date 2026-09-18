//! Day×hour punchcard: a 7×24 grid of spend intensity by weekday and hour-of-day — the
//! "when do I burn tokens?" companion to the calendar heatmap. Pure function of `(weekday, hour,
//! micros)` events; clock-free and deterministic, reusing the heatmap's 0–4 level ramp + glyphs.
//!
//! The store supplies hour metadata when the intake has a timestamp. Older/hourless rows remain an
//! explicit excluded total instead of being assigned a fabricated hour.

use crate::heatmap::level_glyph;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};

/// One (weekday, hour) bucket. `weekday` 0 = Monday … 6 = Sunday; `hour` 0–23 (local). `level` is a
/// 0–4 intensity bucket scaled to the busiest bucket in the grid (0 = no spend).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunchCell {
    pub weekday: u8,
    pub hour: u8,
    pub micros: i64,
    pub level: u8,
}

/// The full 7×24 punchcard. `cells` is always the complete rectangle (168 cells, row-major by
/// weekday then hour) so consumers can render a dense grid without gap-filling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunchcardModel {
    pub cells: Vec<PunchCell>,
    pub max_micros: i64,
    pub total_micros: i64,
    /// Spend from runs with no captured hour (hourless lanes / pre-migration rows) that is NOT in
    /// any cell. Surfaced so the grid total can be reconciled against report/heatmap
    /// instead of silently disagreeing. Defaulted so existing event-only callers + JSON goldens are
    /// unchanged (only `punchcard_from_runs` sets it nonzero).
    #[serde(default)]
    pub excluded_micros: i64,
    #[serde(default)]
    pub excluded_runs: u64,
}

/// Build the punchcard from `(weekday, hour, micros)` events. Out-of-range weekday (>6) / hour (>23)
/// events are skipped (never fabricated); in-range micros sum into their bucket. Levels scale to the
/// busiest bucket (max → 4, zero → 0, proportional 1–3 between) — identical ramp to the heatmap.
pub fn punchcard(events: &[(u8, u8, i64)]) -> PunchcardModel {
    let mut grid = [[0i64; 24]; 7];
    for &(wd, hr, micros) in events {
        if wd < 7 && hr < 24 {
            let cell = &mut grid[wd as usize][hr as usize];
            *cell = cell.saturating_add(micros.max(0));
        }
    }
    let max_micros = grid.iter().flatten().copied().max().unwrap_or(0);
    let total_micros = grid
        .iter()
        .flatten()
        .copied()
        .fold(0i64, i64::saturating_add);
    let mut cells = Vec::with_capacity(7 * 24);
    for (wd, row) in grid.iter().enumerate() {
        for (hr, &micros) in row.iter().enumerate() {
            let level = if micros <= 0 || max_micros <= 0 {
                0
            } else {
                (1 + (micros as i128 * 3 / max_micros as i128) as i64).clamp(1, 4) as u8
            };
            cells.push(PunchCell {
                weekday: wd as u8,
                hour: hr as u8,
                micros,
                level,
            });
        }
    }
    PunchcardModel {
        cells,
        max_micros,
        total_micros,
        excluded_micros: 0,
        excluded_runs: 0,
    }
}

/// Build a day×hour punchcard from captured runs + their `(created_date, hour)` metadata, pricing
/// each run and bucketing by weekday×hour. Lives in core so every surface — the CLI,
/// the serve read route, and the desktop command — shares ONE implementation (tare-tauri can't reach
/// tare-cli, so the bucketing can't live there). Runs whose hour is `None` (hourless lanes /
/// pre-migration rows) are an honest GAP: excluded, never bucketed at a fake hour.
pub fn punchcard_from_runs(
    runs: &[RunRecord],
    day_hours: &std::collections::HashMap<String, (String, Option<u8>)>,
    pricing: &PricingTable,
) -> PunchcardModel {
    let mut events: Vec<(u8, u8, i64)> = Vec::new();
    let mut excluded_micros: i64 = 0;
    let mut excluded_runs: u64 = 0;
    for run in runs {
        // Single-pass sum — not build_report (which runs ~5-6 cause-attribution passes + allocates
        // rows we discard) just to read the total.
        let micros = crate::attribute::total_micros(std::slice::from_ref(run), pricing).max(0);
        let entry = day_hours.get(&run.run_id);
        let Some((date, Some(hour))) = entry else {
            // Unknown run, no stored hour, or unparseable date → NOT bucketed. Track its spend so
            // the caller can reconcile the grid total against report/heatmap, rather
            // than the discrepancy being silent.
            excluded_micros = excluded_micros.saturating_add(micros);
            excluded_runs = excluded_runs.saturating_add(1);
            continue;
        };
        let Some(days) = crate::calendar::parse_date(date).filter(|_| *hour < 24) else {
            excluded_micros = excluded_micros.saturating_add(micros);
            excluded_runs = excluded_runs.saturating_add(1);
            continue;
        };
        let weekday = (days + 3).rem_euclid(7) as u8; // 1970-01-01 = Thursday → 0 = Monday
        events.push((weekday, *hour, micros));
    }
    let mut model = punchcard(&events);
    model.excluded_micros = excluded_micros;
    model.excluded_runs = excluded_runs;
    model
}

/// Render the punchcard as a compact text grid: one row per weekday (Mon…Sun), 24 hour columns.
/// Deterministic. Reuses the heatmap glyph ramp so the two surfaces read consistently.
pub fn render_punchcard_text(model: &PunchcardModel) -> String {
    let labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let mut grid = [[' '; 24]; 7];
    for c in &model.cells {
        if c.weekday < 7 && c.hour < 24 {
            grid[c.weekday as usize][c.hour as usize] = level_glyph(c.level);
        }
    }
    let mut out = String::from("Spend punchcard — ESTIMATE (day × hour, busier = denser)\n\n");
    out.push_str("    0h        6h        12h       18h   23h\n");
    for (i, row) in grid.iter().enumerate() {
        out.push_str(labels[i]);
        out.push(' ');
        out.extend(row.iter());
        out.push('\n');
    }
    // Reconcile: name the spend that isn't on the grid, so the punchcard total can't silently
    // disagree with report/heatmap. Only shown when something was excluded.
    if model.excluded_runs > 0 {
        out.push_str(&format!(
            "\n(excluded: {} run(s), {} — no captured hour; not shown on the grid)\n",
            model.excluded_runs,
            crate::money::MicroUsd(model.excluded_micros).to_dollar_string(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_events_by_weekday_and_hour_and_scales_levels() {
        // Monday 9h busiest, Wednesday 14h small, an out-of-range event ignored.
        let events = vec![
            (0u8, 9u8, 1_000_000i64),
            (0, 9, 200_000), // sums into the same bucket → 1.2M
            (2, 14, 100_000),
            (9, 0, 500_000), // weekday 9 out of range → skipped
        ];
        let m = punchcard(&events);
        assert_eq!(m.cells.len(), 7 * 24, "always the full rectangle");
        let cell = |wd: u8, hr: u8| {
            m.cells
                .iter()
                .find(|c| c.weekday == wd && c.hour == hr)
                .unwrap()
        };
        assert_eq!(cell(0, 9).micros, 1_200_000);
        assert_eq!(m.max_micros, 1_200_000);
        assert_eq!(m.total_micros, 1_300_000); // out-of-range event excluded
        assert_eq!(cell(0, 9).level, 4); // busiest → 4
        assert!((1..=3).contains(&cell(2, 14).level)); // small → mid
        assert_eq!(cell(1, 3).level, 0); // empty bucket

        let nonnegative = punchcard(&[(0, 9, -1_000_000), (0, 9, 50)]);
        let monday_nine = nonnegative
            .cells
            .iter()
            .find(|cell| cell.weekday == 0 && cell.hour == 9)
            .unwrap();
        assert_eq!(
            monday_nine.micros, 50,
            "negative spend cannot cancel a bucket"
        );
    }

    #[test]
    fn text_grid_has_seven_weekday_rows_and_paints_the_busiest() {
        let m = punchcard(&[(0, 9, 1_000_000), (6, 23, 250_000)]);
        let txt = render_punchcard_text(&m);
        assert_eq!(txt.lines().filter(|l| l.starts_with("Mon")).count(), 1);
        assert!(txt.contains("Sun"));
        assert!(txt.contains('█')); // busiest bucket paints the full block
    }

    #[test]
    fn empty_input_is_all_zero_not_a_panic() {
        let m = punchcard(&[]);
        assert_eq!(m.max_micros, 0);
        assert_eq!(m.total_micros, 0);
        assert!(m.cells.iter().all(|c| c.level == 0));
    }

    #[test]
    fn invalid_stored_hours_are_reported_as_excluded() {
        let run = crate::demo::demo_run().unwrap();
        let run_id = run.run_id.clone();
        let pricing =
            PricingTable::from_toml_str(include_str!("../../pricing/pricing.fixture.toml"))
                .unwrap();
        let expected = crate::attribute::total_micros(std::slice::from_ref(&run), &pricing);
        let day_hours =
            std::collections::HashMap::from([(run_id, ("2026-06-01".to_string(), Some(24)))]);
        let model = punchcard_from_runs(&[run], &day_hours, &pricing);
        assert_eq!(model.total_micros, 0);
        assert_eq!(model.excluded_runs, 1);
        assert_eq!(model.excluded_micros, expected);
    }
}
