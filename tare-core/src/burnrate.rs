//! Clock-free run-rate / projected-overrun. Tare classifies period spend
//! backward-looking (budget.rs `period_status`) but never says "at the pace in your captured
//! trend, you'll cross your cap before the period ends." This engine does — WITHOUT a wall clock:
//! it works purely off the period-to-date daily series (`TrendReport.series[].per_day`) and a
//! caller-supplied period length, so it stays deterministic and honest. Every figure is framed as
//! "at your captured pace," never a real-time projection.

use serde::{Deserialize, Serialize};

/// Calendar window offered by the interactive Pulse chart. These are calendar-to-date windows,
/// not rolling durations: `week` starts on Sunday (matching Tare's existing weekly budget),
/// `month` starts on the first, and `year` starts on January 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectionRange {
    Day,
    Week,
    Month,
    Year,
}

impl ProjectionRange {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "day" | "1d" => Ok(Self::Day),
            "week" | "7d" | "wtd" => Ok(Self::Week),
            "month" | "mtd" => Ok(Self::Month),
            "year" | "ytd" => Ok(Self::Year),
            _ => Err(format!(
                "invalid burn-rate range {value:?}; expected day, week, month, or year"
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionWindow {
    pub range: ProjectionRange,
    pub start: String,
    pub end: String,
    pub days_in_period: u32,
}

/// Resolve a named calendar-to-date range around a caller-supplied local civil day. Keeping this in
/// core makes the HTTP and Tauri transports agree exactly, including leap years and week boundaries.
pub fn projection_window(range: &str, today: &str) -> Result<ProjectionWindow, String> {
    let range = ProjectionRange::parse(range)?;
    let today_day = crate::calendar::parse_date(today)
        .ok_or_else(|| format!("invalid local date {today:?}; expected YYYY-MM-DD"))?;
    let (year, month, _) = crate::calendar::civil_from_days(today_day);
    let (start_day, end_day) = match range {
        ProjectionRange::Day => (today_day, today_day),
        ProjectionRange::Week => {
            // 1970-01-01 was a Thursday; Sunday is weekday zero. This retains the established
            // weekly-budget boundary while centralizing it for both transports.
            let weekday_from_sunday = (today_day + 4).rem_euclid(7);
            let start = today_day - weekday_from_sunday;
            (start, start + 6)
        }
        ProjectionRange::Month => {
            let start = crate::calendar::days_from_civil(year, month, 1);
            let next = if month == 12 {
                crate::calendar::days_from_civil(year + 1, 1, 1)
            } else {
                crate::calendar::days_from_civil(year, month + 1, 1)
            };
            (start, next - 1)
        }
        ProjectionRange::Year => (
            crate::calendar::days_from_civil(year, 1, 1),
            crate::calendar::days_from_civil(year, 12, 31),
        ),
    };
    let days_in_period = u32::try_from(end_day - start_day + 1)
        .map_err(|_| format!("invalid {range:?} window around {today:?}"))?;
    Ok(ProjectionWindow {
        range,
        start: crate::calendar::format_date(start_day),
        end: crate::calendar::format_date(end_day),
        days_in_period,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BurnRate {
    /// Median micro-USD of the captured NON-ZERO days (robust to idle days + one-off spikes).
    pub run_rate_micros_per_day: i64,
    /// Median active-day rate discounted by the observed active-day frequency. This is the
    /// calendar-day rate used for the forward projection; it is intentionally distinct from the
    /// active-day median above.
    pub effective_rate_micros_per_day: i64,
    /// Actual captured spend in the elapsed portion of the period.
    pub spent_micros: i64,
    /// Number of elapsed days whose captured spend was non-zero.
    pub active_days: u32,
    /// Actual captured daily spend from period start through the as-of day. Negative inputs are
    /// clamped to zero using the same rule as `spent_micros`, so this is the canonical history the
    /// projection used rather than a rate-derived reconstruction.
    pub daily_spend_micros: Vec<i64>,
    /// Days captured in the period so far (length of the period-to-date daily series).
    pub days_elapsed: u32,
    /// Total days in the period (caller-supplied: 7 for week, days-in-month for month).
    pub days_in_period: u32,
    /// Spent + effective-daily-rate * remaining days, where the effective rate is the active-day
    /// run-rate DISCOUNTED by how often days were active so far (active_days / days_elapsed). This
    /// keeps an intermittent user (e.g. works 2–3 days/week) from being projected as if they'll
    /// spend every remaining calendar day. "At your captured pace" — not a clock projection.
    pub projected_micros: i64,
    /// Low/high projection band: spent + (p25 / p75 of the active daily spend) * remaining. The band
    /// WIDTH is your own daily dispersion (steady pace → tight band, spiky → wide), never a fabricated
    /// confidence interval. Collapses to the point when there are fewer than 2 active days.
    pub projected_low_micros: i64,
    pub projected_high_micros: i64,
    pub cap_micros: i64,
    /// projected <= cap (true when there's no cap).
    pub on_track: bool,
    /// Calendar days until the cap is reached at the effective (frequency-discounted) daily rate;
    /// `None` if the rate is 0 or already over. Agrees with `projected_micros`.
    pub headroom_days: Option<u32>,
}

/// Project period spend from the period-to-date daily series. `per_day` MUST be the period's
/// elapsed days (from period start through the last captured day); `days_in_period` is the full
/// period length. Pure, integer, clock-free.
pub fn project(per_day: &[i64], days_in_period: u32, cap_micros: i64) -> BurnRate {
    let daily_spend_micros: Vec<i64> = per_day.iter().copied().map(|x| x.max(0)).collect();
    let spent = daily_spend_micros
        .iter()
        .copied()
        .fold(0i64, i64::saturating_add);
    let days_elapsed = u32::try_from(daily_spend_micros.len()).unwrap_or(u32::MAX);

    // Run-rate = median of the days that actually had spend (idle days don't drag it to ~0, and a
    // single spike doesn't dominate as a mean would).
    let mut active: Vec<i64> = daily_spend_micros
        .iter()
        .copied()
        .filter(|&x| x > 0)
        .collect();
    active.sort_unstable();
    let run_rate = if active.is_empty() {
        0
    } else {
        active[active.len() / 2]
    };

    let remaining = days_in_period.saturating_sub(days_elapsed);
    // Effective per-CALENDAR-day rate: the active-day run-rate discounted by how often days were
    // active so far (active_days / days_elapsed). Projecting an active-day rate across ALL remaining
    // calendar days over-projects an intermittent user (weekday-only, or 2–3 days/week) as if they'd
    // spend every day. Scaling by the observed active-day frequency keeps the
    // forward projection consistent with captured behavior.
    let active_days = active.len() as u32;
    let effective = |rate: i64| -> i64 {
        if days_elapsed == 0 {
            0
        } else {
            (rate as i128 * active_days as i128 / days_elapsed as i128) as i64
        }
    };
    let effective_rate_micros_per_day = effective(run_rate);
    let project_at =
        |rate: i64| spent.saturating_add(effective(rate).saturating_mul(remaining as i64));
    let projected_micros = project_at(run_rate);
    // Band from the p25/p75 of the ACTIVE daily spend (order statistics — deterministic, integer).
    // Fewer than 2 active days → no dispersion to speak of, so the band collapses to the point.
    let (lo_rate, hi_rate) = if active.len() < 2 {
        (run_rate, run_rate)
    } else {
        (active[active.len() / 4], active[(active.len() * 3) / 4])
    };
    let projected_low_micros = project_at(lo_rate);
    let projected_high_micros = project_at(hi_rate);
    let on_track = cap_micros <= 0 || projected_micros <= cap_micros;
    // Headroom in CALENDAR days uses the same effective rate as the projection, so it agrees with
    // projected_micros for an intermittent user rather than reading the active-day rate.
    let headroom_days = if effective_rate_micros_per_day > 0 && cap_micros > spent {
        // Saturate rather than wrap: a large cap-minus-spent over a tiny effective
        // rate can exceed u32, and `as u32` would silently produce a bogus small figure.
        Some(
            u32::try_from((cap_micros - spent) / effective_rate_micros_per_day).unwrap_or(u32::MAX),
        )
    } else {
        None
    };

    BurnRate {
        run_rate_micros_per_day: run_rate,
        effective_rate_micros_per_day,
        spent_micros: spent,
        active_days,
        daily_spend_micros,
        days_elapsed,
        days_in_period,
        projected_micros,
        projected_low_micros,
        projected_high_micros,
        cap_micros,
        on_track,
        headroom_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_projection_windows_cover_day_week_month_and_leap_year() {
        assert_eq!(
            projection_window("day", "2026-09-17").unwrap(),
            ProjectionWindow {
                range: ProjectionRange::Day,
                start: "2026-09-17".into(),
                end: "2026-09-17".into(),
                days_in_period: 1,
            }
        );
        assert_eq!(
            projection_window("week", "2026-09-17").unwrap(),
            ProjectionWindow {
                range: ProjectionRange::Week,
                start: "2026-09-13".into(),
                end: "2026-09-19".into(),
                days_in_period: 7,
            }
        );
        assert_eq!(
            projection_window("mtd", "2024-02-29").unwrap(),
            ProjectionWindow {
                range: ProjectionRange::Month,
                start: "2024-02-01".into(),
                end: "2024-02-29".into(),
                days_in_period: 29,
            }
        );
        let leap_year = projection_window("ytd", "2024-02-29").unwrap();
        assert_eq!(leap_year.start, "2024-01-01");
        assert_eq!(leap_year.end, "2024-12-31");
        assert_eq!(leap_year.days_in_period, 366);
    }

    #[test]
    fn projection_window_rejects_unknown_ranges_and_impossible_dates() {
        assert!(projection_window("quarter", "2026-09-17").is_err());
        assert!(projection_window("month", "2026-02-31").is_err());
    }

    #[test]
    fn steady_spend_under_cap_is_on_track() {
        // 5 elapsed days at ~$1/day, 30-day month, $50 cap -> ~$30 projected, on track.
        let per_day = vec![1_000_000, 1_000_000, 0, 1_000_000, 1_000_000];
        let b = project(&per_day, 30, 50_000_000);
        assert_eq!(
            b.run_rate_micros_per_day, 1_000_000,
            "median of the active days"
        );
        assert_eq!(b.days_elapsed, 5);
        assert_eq!(b.spent_micros, 4_000_000);
        assert_eq!(b.active_days, 4);
        assert_eq!(
            b.daily_spend_micros,
            vec![1_000_000, 1_000_000, 0, 1_000_000, 1_000_000]
        );
        assert_eq!(b.effective_rate_micros_per_day, 800_000);
        // 4 of 5 days were active, so the forward projection uses the effective calendar rate
        // 1M * 4/5 = 800k, NOT the raw active-day 1M: spent 4M + 800k * 25
        // remaining = 24M < 50M cap.
        assert_eq!(b.projected_micros, 24_000_000);
        assert!(b.on_track);
        assert!(b.headroom_days.is_some());
    }

    #[test]
    fn intermittent_user_is_not_projected_as_spending_every_day() {
        // Works ~2 of every 5 elapsed days (10 days, 4 active at $5 each = $20 spent). Naively,
        // 5M * 20 remaining = +100M → 120M. Discounted by the 4/10 active frequency it is
        // 5M * 4/10 = 2M/calendar-day → +2M * 20 = 40M → 60M projected. The honest figure sits far
        // below the "spends every day" over-projection.
        let per_day = vec![5_000_000, 0, 0, 5_000_000, 0, 5_000_000, 0, 0, 0, 5_000_000];
        let b = project(&per_day, 30, 0);
        assert_eq!(
            b.run_rate_micros_per_day, 5_000_000,
            "active-day median unchanged"
        );
        assert_eq!(b.projected_micros, 60_000_000);
        // The naive active-day projection would have read 120M — nearly double.
        assert!(b.projected_micros < 20_000_000 + 5_000_000 * 20);
    }

    #[test]
    fn hostile_daily_totals_saturate_instead_of_wrapping() {
        let b = project(&[i64::MAX, i64::MAX], 30, i64::MAX);
        assert_eq!(b.run_rate_micros_per_day, i64::MAX);
        assert_eq!(b.projected_micros, i64::MAX);
        assert_eq!(b.projected_low_micros, i64::MAX);
        assert_eq!(b.projected_high_micros, i64::MAX);
        assert_eq!(b.spent_micros, i64::MAX);
        assert!(b.on_track);
    }

    #[test]
    fn reported_history_uses_the_same_nonnegative_values_as_the_projection() {
        let b = project(&[1_000_000, -9_000_000, 3_000_000], 3, 0);
        assert_eq!(b.daily_spend_micros, vec![1_000_000, 0, 3_000_000]);
        assert_eq!(b.spent_micros, 4_000_000);
        assert_eq!(b.active_days, 2);
        assert!(b.projected_low_micros >= b.spent_micros);
        assert!(b.projected_micros >= b.spent_micros);
        assert!(b.projected_high_micros >= b.spent_micros);
    }

    #[test]
    fn projects_over_cap_even_while_spent_is_under() {
        // Spent only $8 of a $10 cap on day 2 of 30, but the pace blows the cap out.
        let per_day = vec![4_000_000, 4_000_000];
        let b = project(&per_day, 30, 10_000_000);
        assert!(b.projected_micros > b.cap_micros);
        assert!(!b.on_track, "on pace to exceed the cap before period end");
    }

    #[test]
    fn band_widens_with_daily_dispersion() {
        // Spiky spend: active days [1,2,5,8]M, day 4 of 30. p25=2M, median=5M, p75=8M.
        // remaining = 26. spent = 16M.
        let per_day = vec![1_000_000, 2_000_000, 5_000_000, 8_000_000];
        let b = project(&per_day, 30, 0);
        assert_eq!(b.run_rate_micros_per_day, 5_000_000);
        assert_eq!(b.projected_micros, 16_000_000 + 5_000_000 * 26);
        assert_eq!(b.projected_low_micros, 16_000_000 + 2_000_000 * 26);
        assert_eq!(b.projected_high_micros, 16_000_000 + 8_000_000 * 26);
        assert!(b.projected_low_micros < b.projected_micros);
        assert!(b.projected_high_micros > b.projected_micros);
    }

    #[test]
    fn band_collapses_to_the_point_with_one_active_day() {
        let b = project(&[3_000_000], 7, 0);
        assert_eq!(b.projected_low_micros, b.projected_micros);
        assert_eq!(b.projected_high_micros, b.projected_micros);
    }

    #[test]
    fn no_spend_or_no_cap_is_on_track() {
        assert!(project(&[], 30, 10_000_000).on_track);
        assert_eq!(project(&[], 30, 10_000_000).run_rate_micros_per_day, 0);
        // No cap -> always on track, headroom undefined.
        let b = project(&[5_000_000], 7, 0);
        assert!(b.on_track);
        assert!(b.headroom_days.is_none());
    }
}
