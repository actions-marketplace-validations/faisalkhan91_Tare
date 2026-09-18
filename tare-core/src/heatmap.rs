//! Calendar heatmap: a GitHub-contributions-style grid of daily spend intensity — the
//! at-a-glance cadence view for the daily-return habit surface. Pure function of a daily (date,
//! micros) series; clock-free and deterministic.
//!
//! The day×hour view now lives separately in `punchcard`; this module remains the shared
//! day-intensity model used by the CLI, browser, and desktop surfaces.

use crate::calendar::parse_date;
use crate::model::RunRecord;
use crate::pricing::PricingTable;
use serde::{Deserialize, Serialize};

/// One day in the grid. `level` is a 0–4 intensity bucket (0 = no spend; 1–4 scale with the busiest
/// day in the window), and `weekday`/`week` are its position (Monday-first rows, week columns).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeatCell {
    pub date: String,
    pub micros: i64,
    pub level: u8,
    /// 0 = Monday … 6 = Sunday.
    pub weekday: u8,
    /// Column index (weeks since the first day's Monday).
    pub week: u32,
}

/// The heatmap over a window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeatmapModel {
    pub cells: Vec<HeatCell>,
    pub max_micros: i64,
    pub total_micros: i64,
    /// Number of week columns (0 when empty).
    pub weeks: u32,
}

/// 1970-01-01 (epoch day 0) was a Thursday; Monday-first index = (day + 3) mod 7.
fn weekday_mon0(epoch_day: i64) -> u8 {
    (epoch_day + 3).rem_euclid(7) as u8
}

/// Build the calendar heatmap from a chronological daily `(date, micros)` series. Days that don't
/// parse are skipped (never fabricated). Levels scale to the busiest day: the max-spend day is 4,
/// zero-spend days are 0, and everything in between is a proportional 1–3.
pub fn calendar_heatmap(days: &[(String, i64)]) -> HeatmapModel {
    // Aggregate ONLY over days whose date parses — the same set that becomes cells.
    // Folding max/total over the raw input would let an unparseable-date row inflate total_micros (so
    // Σ cells.micros != total) or the level-ramp max (so the darkest shade maps to no visible cell).
    let max_micros = days
        .iter()
        .filter(|(d, _)| parse_date(d).is_some())
        .map(|(_, m)| *m)
        .max()
        .unwrap_or(0);
    let total_micros = days
        .iter()
        .filter(|(d, _)| parse_date(d).is_some())
        .map(|(_, m)| *m)
        .fold(0i64, i64::saturating_add);
    // Column origin: the Monday on/before the first parseable day.
    let first_monday = days
        .iter()
        .filter_map(|(d, _)| parse_date(d))
        .min()
        .map(|d| d - weekday_mon0(d) as i64);

    let mut cells = Vec::new();
    let mut weeks = 0u32;
    for (date, micros) in days {
        let Some(day) = parse_date(date) else {
            continue;
        };
        let level = if *micros <= 0 || max_micros <= 0 {
            0
        } else {
            // 1..=4, proportional to the busiest day (max → 4).
            (1 + (*micros as i128 * 3 / max_micros as i128) as i64).clamp(1, 4) as u8
        };
        let origin = first_monday.unwrap_or(day);
        let week = ((day - origin) / 7).max(0) as u32;
        weeks = weeks.max(week.saturating_add(1));
        cells.push(HeatCell {
            date: date.clone(),
            micros: *micros,
            level,
            weekday: weekday_mon0(day),
            week,
        });
    }
    HeatmapModel {
        cells,
        max_micros,
        total_micros,
        weeks,
    }
}

/// Build the calendar heatmap directly from dated runs: price each run, sum by date,
/// fill the zero days across the span, apply an optional trailing-`window` (last N days), then hand
/// off to [`calendar_heatmap`]. Lives in core so the CLI, the serve route, and the desktop command
/// share ONE implementation (tare-tauri can't reach tare-cli). `dated` is `(run, created_date)`.
pub fn heatmap_from_runs(
    dated: &[(RunRecord, String)],
    pricing: &PricingTable,
    window: Option<usize>,
) -> HeatmapModel {
    use std::collections::BTreeMap;
    let mut micros: BTreeMap<String, i64> = BTreeMap::new();
    for (run, date) in dated {
        // Single-pass sum, not a full build_report per run just to read the total.
        let m = crate::attribute::total_micros(std::slice::from_ref(run), pricing);
        let entry = micros.entry(date.clone()).or_insert(0);
        *entry = entry.saturating_add(m);
    }
    let (Some(from), Some(to)) = (
        micros.keys().next().cloned(),
        micros.keys().next_back().cloned(),
    ) else {
        return calendar_heatmap(&[]);
    };
    let mut series: Vec<(String, i64)> = crate::calendar::days_between(&from, &to)
        .into_iter()
        .map(|date| {
            let m = micros.get(&date).copied().unwrap_or(0);
            (date, m)
        })
        .collect();
    if let Some(n) = window {
        if series.len() > n {
            series.drain(0..series.len() - n);
        }
    }
    calendar_heatmap(&series)
}

/// Glyph for an intensity level (0–4). Space for empty, ramping blocks for spend.
pub fn level_glyph(level: u8) -> char {
    match level {
        0 => '·',
        1 => '▪',
        2 => '▤',
        3 => '▦',
        _ => '█',
    }
}

/// Render the heatmap as a compact text grid: one row per weekday (Mon…Sun), one column per week.
/// Deterministic; a missing (date-less) cell in the rectangle shows as a blank.
pub fn render_heatmap_text(model: &HeatmapModel) -> String {
    let labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    // Index cells by (weekday, week) for O(1) grid fill.
    let mut grid = vec![vec![' '; model.weeks as usize]; 7];
    for c in &model.cells {
        if (c.week as usize) < model.weeks as usize {
            grid[c.weekday as usize][c.week as usize] = level_glyph(c.level);
        }
    }
    let mut out = String::from("Spend heatmap — ESTIMATE (busier = denser)\n\n");
    for (i, row) in grid.iter().enumerate() {
        out.push_str(labels[i]);
        out.push(' ');
        out.extend(row.iter());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_mapping_is_monday_first() {
        // 2026-06-22 is a Monday.
        let mon = parse_date("2026-06-22").unwrap();
        assert_eq!(weekday_mon0(mon), 0);
        assert_eq!(weekday_mon0(mon + 6), 6); // Sunday
                                              // Epoch day 0 (1970-01-01) is a Thursday → index 3.
        assert_eq!(weekday_mon0(0), 3);
    }

    #[test]
    fn levels_scale_to_the_busiest_day_and_zeros_are_empty() {
        let days = vec![
            ("2026-06-22".to_string(), 0),         // Mon, empty
            ("2026-06-23".to_string(), 100_000),   // small
            ("2026-06-24".to_string(), 1_000_000), // busiest → level 4
        ];
        let m = calendar_heatmap(&days);
        assert_eq!(m.max_micros, 1_000_000);
        assert_eq!(m.total_micros, 1_100_000);
        let by_date = |d: &str| m.cells.iter().find(|c| c.date == d).unwrap().level;
        assert_eq!(by_date("2026-06-22"), 0);
        assert_eq!(by_date("2026-06-24"), 4);
        assert!((1..=3).contains(&by_date("2026-06-23")));
        // All three fall in the same week column (Mon-anchored).
        assert_eq!(m.weeks, 1);
    }

    #[test]
    fn text_grid_has_seven_weekday_rows() {
        let days = vec![
            ("2026-06-22".to_string(), 500_000),
            ("2026-06-29".to_string(), 250_000), // next Monday → second column
        ];
        let m = calendar_heatmap(&days);
        assert_eq!(m.weeks, 2);
        let txt = render_heatmap_text(&m);
        assert_eq!(txt.lines().filter(|l| l.starts_with("Mon")).count(), 1);
        assert!(txt.contains("Sun"));
        // The busiest day paints the full block.
        assert!(txt.contains('█'));
    }

    #[test]
    fn daily_total_saturates_for_extreme_values() {
        let days = vec![
            ("2026-06-22".to_string(), i64::MAX),
            ("2026-06-23".to_string(), i64::MAX),
        ];
        let model = calendar_heatmap(&days);
        assert_eq!(model.total_micros, i64::MAX);
        assert_eq!(model.max_micros, i64::MAX);
        assert!(model.cells.iter().all(|cell| cell.level == 4));
    }
}
