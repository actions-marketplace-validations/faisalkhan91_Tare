//! Pure civil-date arithmetic (Howard Hinnant's algorithms). NO clock, NO RNG — dates enter
//! as `YYYY-MM-DD` strings (the store's `created_date` column) so the core stays clock-free
//! and every trend is deterministic.

/// Days since 1970-01-01 for a civil (y, m, d). Inverse of [`civil_from_days`].
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    // The public helper is also used with derived years, so do the arithmetic in i128 and
    // saturate only if a caller supplies a year whose day index cannot fit in i64.
    let y = i128::from(y);
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i128::from(m);
    let d = i128::from(d);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    saturating_i128_to_i64(era * 146_097 + doe - 719_468)
}

/// Civil (y, m, d) for days since 1970-01-01. Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = i128::from(z) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (saturating_i128_to_i64(if m <= 2 { y + 1 } else { y }), m, d)
}

fn saturating_i128_to_i64(value: i128) -> i64 {
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Parse `YYYY-MM-DD` to days-since-epoch. Rejects impossible dates (e.g. `2026-02-31`) via a
/// civil round-trip rather than silently rolling them over to the wrong day.
pub fn parse_date(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() != 10
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    // Year bound keeps `days_from_civil` well away from overflow and matches `{y:04}` output.
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    // Reject day-of-month overflows (Feb 31, Apr 31, ...) that civil arithmetic would roll over.
    if civil_from_days(days) != (y, m, d) {
        return None;
    }
    Some(days)
}

/// Format days-since-epoch as `YYYY-MM-DD`.
pub fn format_date(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Dense inclusive list of `YYYY-MM-DD` from `from` to `to`. Empty if `from > to` or either
/// is unparseable. Bounded so a corrupt range can't allocate without limit.
pub fn days_between(from: &str, to: &str) -> Vec<String> {
    let (Some(a), Some(b)) = (parse_date(from), parse_date(to)) else {
        return Vec::new();
    };
    if a > b || (b - a) > 366 * 50 {
        return Vec::new();
    }
    (a..=b).map(format_date).collect()
}

/// The Monday (ISO week start) of the week containing `date`, as `YYYY-MM-DD`. `None` if unparseable
/// (weekly-digest cadence). 1970-01-01 was a Thursday, so weekday-from-Monday is
/// `(days + 3) mod 7` with Monday = 0.
pub fn week_start(date: &str) -> Option<String> {
    let days = parse_date(date)?;
    let weekday_from_monday = (days + 3).rem_euclid(7); // 0 = Monday
    Some(format_date(days - weekday_from_monday))
}

/// The `YYYY-MM-DD` civil day for a unix-SECONDS instant shifted by `offset_minutes` (the user's
/// local-day offset; 0 = UTC). `div_euclid` stays correct for negative (west-of-UTC) offsets near
/// midnight/epoch. This is the shared bucketing rule so a stamped day and a "today"/trend query
/// always agree.
pub fn civil_date_for(secs: i64, offset_minutes: i64) -> String {
    let shifted = i128::from(secs) + i128::from(offset_minutes) * 60;
    format_date(saturating_i128_to_i64(shifted.div_euclid(86_400)))
}

/// RFC3339 UTC timestamp for a unix-seconds instant (e.g. `2026-07-10T02:15:30Z`). Pure — the
/// clock-owning caller supplies `secs`; shared by every transport that stamps `refreshed_at` so the
/// format can't drift. Uses the same civil-date rule as [`civil_date_for`] for the date portion.
pub fn rfc3339_utc(secs: i64) -> String {
    let date = civil_date_for(secs, 0);
    let tod = secs.rem_euclid(86_400);
    format!(
        "{date}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Parse an ISO-8601 / RFC-3339 timestamp to unix SECONDS (UTC). Handles
/// `YYYY-MM-DD(T| )HH:MM[:SS][.frac][Z|±HH:MM|±HHMM]`. A trailing `Z`/`z` or an absent zone is
/// treated as UTC; an explicit signed offset is applied so the result is ALWAYS UTC. Pure and
/// clock-free; returns `None` on anything it can't confidently parse (callers keep a raw fallback).
pub fn parse_iso8601_to_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    let days = parse_date(s.get(0..10)?)?;
    match s.as_bytes().get(10) {
        Some(b'T') | Some(b't') | Some(b' ') => {}
        _ => return None,
    }
    let t = s.get(11..)?;
    let bytes = t.as_bytes();
    let hh = two_digits(bytes, 0)?;
    if bytes.get(2) != Some(&b':') {
        return None;
    }
    let mm = two_digits(bytes, 3)?;
    // Seconds are optional (`HH:MM` is valid RFC-3339-ish). A leap second (60) is clamped to 59.
    let (ss, mut cursor, has_seconds) = if bytes.get(5) == Some(&b':') {
        (two_digits(bytes, 6)?, 8, true)
    } else {
        (0, 5, false)
    };
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }

    if bytes.get(cursor) == Some(&b'.') {
        if !has_seconds {
            return None;
        }
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return None;
        }
    }

    let zone_min = zone_offset_minutes(t.get(cursor..)?)?;
    let base = days * 86_400 + hh * 3_600 + mm * 60 + ss.min(59);
    Some(base - zone_min * 60)
}

fn two_digits(bytes: &[u8], start: usize) -> Option<i64> {
    let tens = *bytes.get(start)?;
    let ones = *bytes.get(start + 1)?;
    if !tens.is_ascii_digit() || !ones.is_ascii_digit() {
        return None;
    }
    Some(i64::from(tens - b'0') * 10 + i64::from(ones - b'0'))
}

/// The zone offset in minutes for the suffix after the parsed time and optional fraction.
/// `Z`/`z` or no zone → 0; `±HH:MM` / `±HHMM` / `±HH` → signed minutes.
fn zone_offset_minutes(zone: &str) -> Option<i64> {
    if zone.is_empty() || matches!(zone, "Z" | "z") {
        return Some(0);
    }
    let bytes = zone.as_bytes();
    let sign = match bytes.first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return None,
    };
    let (zh, zm) = match bytes.len() {
        3 => (two_digits(bytes, 1)?, 0),
        5 => (two_digits(bytes, 1)?, two_digits(bytes, 3)?),
        6 if bytes.get(3) == Some(&b':') => (two_digits(bytes, 1)?, two_digits(bytes, 4)?),
        _ => return None,
    };
    if zh > 23 || zm > 59 {
        return None;
    }
    Some(sign * (zh * 60 + zm))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_for_buckets_into_the_offset_local_day() {
        // 2026-06-02T05:00:00Z is 2026-06-01 21:00 at offset -480 (Pacific) → the PRIOR local day.
        let z = parse_iso8601_to_secs("2026-06-02T05:00:00Z").unwrap();
        assert_eq!(civil_date_for(z, 0), "2026-06-02"); // UTC day
        assert_eq!(civil_date_for(z, -480), "2026-06-01"); // local day west of UTC
                                                           // Midday UTC is the same civil day under a modest offset.
        let noon = parse_iso8601_to_secs("2026-06-02T12:00:00Z").unwrap();
        assert_eq!(civil_date_for(noon, -480), "2026-06-02");
    }

    #[test]
    fn parse_iso8601_variants() {
        // Z, fractional seconds, no seconds, space separator, explicit offsets.
        let base = parse_iso8601_to_secs("2026-06-02T00:00:00Z").unwrap();
        assert_eq!(
            parse_iso8601_to_secs("2026-06-02T00:00:00.123Z"),
            Some(base)
        );
        assert_eq!(parse_iso8601_to_secs("2026-06-02T00:00Z"), Some(base));
        assert_eq!(parse_iso8601_to_secs("2026-06-02 00:00:00"), Some(base)); // naive → UTC
                                                                              // 07:00 at -07:00 is 14:00 UTC.
        assert_eq!(
            parse_iso8601_to_secs("2026-06-02T07:00:00-07:00"),
            parse_iso8601_to_secs("2026-06-02T14:00:00Z")
        );
        // 05:30 at +05:30 is 00:00 UTC same day.
        assert_eq!(
            parse_iso8601_to_secs("2026-06-02T05:30:00+05:30"),
            parse_iso8601_to_secs("2026-06-02T00:00:00Z")
        );
        // Compact zone form ±HHMM.
        assert_eq!(
            parse_iso8601_to_secs("2026-06-02T07:00:00-0700"),
            parse_iso8601_to_secs("2026-06-02T14:00:00Z")
        );
        assert_eq!(
            parse_iso8601_to_secs("2026-06-02T07:00:00-07"),
            parse_iso8601_to_secs("2026-06-02T14:00:00Z")
        );
        // Garbage → None (caller keeps its raw fallback).
        assert!(parse_iso8601_to_secs("not-a-timestamp").is_none());
        assert!(parse_iso8601_to_secs("2026-06-02").is_none()); // date only, no time
        assert!(parse_iso8601_to_secs("2026-13-02T00:00:00Z").is_none()); // bad month
    }

    #[test]
    fn parse_iso8601_rejects_negative_fields_and_trailing_garbage() {
        for value in [
            "2026-06-02T-1:00:00Z",
            "2026-06-02T00:-1:00Z",
            "2026-06-02T00:00:-1Z",
            "2026-06-02T00:00:00garbage",
            "2026-06-02T00:00:00garbageZ",
            "2026-06-02T00:00:00Zgarbage",
            "2026-06-02T00:00:00+05:30garbage",
            "2026-06-02T00:00:.5Z",
            "2026-06-02T00:00:00.Z",
        ] {
            assert!(
                parse_iso8601_to_secs(value).is_none(),
                "unexpectedly parsed {value:?}"
            );
        }
    }

    #[test]
    fn week_start_snaps_to_monday() {
        // 2026-07-04 is a Saturday → Monday of that week is 2026-06-29.
        assert_eq!(week_start("2026-07-04").as_deref(), Some("2026-06-29"));
        // A Monday maps to itself; the following Sunday maps back to the same Monday.
        assert_eq!(week_start("2026-06-29").as_deref(), Some("2026-06-29"));
        assert_eq!(week_start("2026-07-05").as_deref(), Some("2026-06-29"));
        // Next Monday rolls to the new week.
        assert_eq!(week_start("2026-07-06").as_deref(), Some("2026-07-06"));
        assert!(week_start("nope").is_none());
    }

    #[test]
    fn round_trips_civil() {
        for s in ["1970-01-01", "2026-06-24", "2000-02-29", "1999-12-31"] {
            let days = parse_date(s).unwrap();
            assert_eq!(format_date(days), s);
        }
    }

    #[test]
    fn days_between_cases() {
        assert_eq!(days_between("2026-06-24", "2026-06-24"), vec!["2026-06-24"]);
        // Month boundary.
        assert_eq!(
            days_between("2026-01-30", "2026-02-02"),
            vec!["2026-01-30", "2026-01-31", "2026-02-01", "2026-02-02"]
        );
        // from > to -> empty.
        assert!(days_between("2026-06-25", "2026-06-24").is_empty());
        // Unparseable -> empty.
        assert!(days_between("nope", "2026-06-24").is_empty());
    }

    #[test]
    fn rejects_impossible_dates() {
        // Field-valid but calendar-impossible dates are rejected, not rolled over.
        assert!(parse_date("2026-02-31").is_none());
        assert!(parse_date("2026-04-31").is_none());
        assert!(parse_date("2025-02-29").is_none()); // 2025 is not a leap year
        assert!(parse_date("2024-02-29").is_some()); // 2024 is
        assert!(parse_date("0000-01-01").is_none()); // year bound
        assert!(parse_date("2026-13-01").is_none());
        assert!(parse_date("2026-6-01").is_none()); // canonical shape is required
        assert!(parse_date("2026-06-1").is_none());
    }

    #[test]
    fn civil_arithmetic_handles_numeric_extremes() {
        assert_eq!(days_from_civil(i64::MIN, 1, 1), i64::MIN);
        assert_eq!(days_from_civil(i64::MAX, 12, 31), i64::MAX);

        for days in [i64::MIN, i64::MAX] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }

        // Extreme offsets used to overflow before the day division.
        assert!(!civil_date_for(i64::MAX, i64::MAX).is_empty());
        assert!(!civil_date_for(i64::MIN, i64::MIN).is_empty());
    }
}
