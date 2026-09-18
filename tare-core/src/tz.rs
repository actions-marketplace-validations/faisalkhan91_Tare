//! Authoritative IANA timezone bucketing. Backed by jiff's bundled tz
//! database (`tzdb-bundle-always`), so zone validation and DST-aware date bucketing are OFFLINE and
//! deterministic across macOS/Windows/Linux — no reliance on the host `/usr/share/zoneinfo`. Pure
//! given inputs: no clock is read; the caller supplies the instant. This is the one place that turns
//! an absolute instant into a local calendar day, so the bucketing rule can't drift.

/// True if `tz` resolves to a real zone in the bundled IANA database. `UTC` is always valid; a
/// well-formed-but-nonexistent identifier (e.g. `Not/AZone`) is rejected, unlike a format-only check.
pub fn is_valid_zone(tz: &str) -> bool {
    jiff::tz::TimeZone::get(tz).is_ok()
}

/// The `YYYY-MM-DD` civil day that `unix_nanos` (nanoseconds since the Unix epoch, UTC) falls on in
/// IANA zone `tz`, honoring DST/offset transitions. Rows with `start_unix_nano` are bucketed in the
/// requested timezone. `Err` when the zone is unknown or the instant is out of range.
pub fn zone_local_date(unix_nanos: i128, tz: &str) -> Result<String, String> {
    let ts = jiff::Timestamp::from_nanosecond(unix_nanos)
        .map_err(|e| format!("timestamp {unix_nanos}ns out of range: {e}"))?;
    let zone =
        jiff::tz::TimeZone::get(tz).map_err(|e| format!("invalid IANA timezone {tz:?}: {e}"))?;
    Ok(ts.to_zoned(zone).date().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::parse_iso8601_to_secs;

    /// Nanoseconds since the Unix epoch for an ISO-8601 UTC instant (test convenience).
    fn nanos(iso: &str) -> i128 {
        parse_iso8601_to_secs(iso).expect("parse iso") as i128 * 1_000_000_000
    }

    #[test]
    fn validates_against_the_bundled_iana_database() {
        assert!(is_valid_zone("UTC"));
        assert!(is_valid_zone("America/Los_Angeles"));
        assert!(is_valid_zone("Australia/Lord_Howe")); // 30-min DST zone
                                                       // Well-formed but nonexistent -> rejected (format-only validation would have passed these).
        assert!(!is_valid_zone("Not/AZone"));
        assert!(!is_valid_zone("America/Nowhere"));
        assert!(!is_valid_zone(""));
    }

    #[test]
    fn buckets_by_dst_aware_local_date() {
        // Same wall-clock UTC instant (07:30Z), opposite seasons: DST (PDT, UTC-7) in summer keeps
        // it on the UTC day; standard time (PST, UTC-8) in winter pushes it to the prior day. This
        // only holds if the offset is chosen by the actual DST rule, not a fixed offset.
        assert_eq!(
            zone_local_date(nanos("2026-07-01T07:30:00Z"), "America/Los_Angeles").unwrap(),
            "2026-07-01" // PDT -7 -> 00:30 local, same day
        );
        assert_eq!(
            zone_local_date(nanos("2026-01-01T07:30:00Z"), "America/Los_Angeles").unwrap(),
            "2025-12-31" // PST -8 -> 23:30 prior day
        );
        // UTC is the identity bucket.
        assert_eq!(
            zone_local_date(nanos("2026-07-01T07:30:00Z"), "UTC").unwrap(),
            "2026-07-01"
        );
        // East of UTC can advance to the next local day.
        assert_eq!(
            zone_local_date(nanos("2026-07-01T20:00:00Z"), "Asia/Tokyo").unwrap(),
            "2026-07-02" // JST +9 -> 05:00 next day
        );
    }

    #[test]
    fn spring_forward_instant_buckets_on_the_transition_day() {
        // US spring-forward 2026-03-08: 02:00 PST jumps to 03:00 PDT (09:00 -> 10:00 UTC). An instant
        // just after the jump is still 2026-03-08 locally.
        assert_eq!(
            zone_local_date(nanos("2026-03-08T10:30:00Z"), "America/Los_Angeles").unwrap(),
            "2026-03-08" // PDT -7 -> 03:30 local
        );
    }

    #[test]
    fn unknown_zone_is_an_error_not_a_panic() {
        assert!(zone_local_date(0, "Not/AZone").is_err());
    }
}
