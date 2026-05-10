//! Tiny ISO-8601 helpers shared by [`crate::session`] and
//! [`crate::claude_telemetry`].
//!
//! Both call sites need to (a) parse the narrow ISO-8601 subset our
//! own writers and Claude's CLI produce, and (b) convert between Unix
//! seconds and a printable UTC string. Until this module landed both
//! had near-identical copies, with predictable risk of drift —
//! consolidated here so a fix in one place reaches both.
//!
//! Scope: UTC only, with `Z` / `+00:00` / `-00:00` suffix or no suffix.
//! Non-UTC offsets return `None` because retention-style callers prefer
//! "leave it alone" over "guess the wrong direction". Fractional seconds
//! must be ASCII digits — float spellings like `NaN` / `1e3` would
//! otherwise yield NaN seconds and poison `partial_cmp`-based sorts.

#![allow(dead_code)]

use std::time::{SystemTime, UNIX_EPOCH};

/// Parse the ISO-8601 subset CodeScope and Claude Code emit
/// (`YYYY-MM-DDTHH:MM:SS` optionally followed by `.fff` and a `Z` /
/// `+00:00` / `-00:00` suffix) into seconds since the Unix epoch.
///
/// Validation is strict: month / day / hour / minute / second must be
/// in their canonical ranges (including leap-aware day-of-month), and
/// the fractional component must be ASCII digits only. Anything else
/// returns `None`. Callers that feed retention pruning rely on this
/// "fail closed" behaviour so a corrupted timestamp doesn't trigger an
/// unexpected drop.
pub fn parse_iso8601_secs(s: &str) -> Option<f64> {
    let s = if let Some(stripped) = s.strip_suffix('Z') {
        stripped
    } else if let Some(stripped) = s.strip_suffix("+00:00") {
        stripped
    } else if let Some(stripped) = s.strip_suffix("-00:00") {
        stripped
    } else {
        // No UTC marker — treat as already-UTC. Hand-edited fixtures
        // sometimes drop the suffix.
        s
    };

    let t_pos = s.find('T')?;
    let (date, time_full) = (&s[..t_pos], &s[t_pos + 1..]);

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let (hms, frac) = match time_full.find('.') {
        Some(dot) => (&time_full[..dot], &time_full[dot + 1..]),
        None => (time_full, ""),
    };
    let mut hms_parts = hms.split(':');
    let hour: i64 = hms_parts.next()?.parse().ok()?;
    let minute: i64 = hms_parts.next()?.parse().ok()?;
    let sec: i64 = hms_parts.next()?.parse().ok()?;
    if hms_parts.next().is_some() {
        return None;
    }
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) || !(0..=60).contains(&sec) {
        // Allow sec == 60 for leap-second wallclock readings; reject
        // the rest.
        return None;
    }

    let days = days_since_epoch(year, month, day)?;
    let mut total = days as f64 * 86_400.0
        + hour as f64 * 3_600.0
        + minute as f64 * 60.0
        + sec as f64;

    if !frac.is_empty() {
        // ASCII-digit-only — `f64::from_str` would otherwise accept
        // `NaN`, `inf`, scientific notation, signs, etc. and yield a
        // value that breaks `partial_cmp` ordering downstream.
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: f64 = frac.parse().ok()?;
        total += n / 10f64.powi(frac.len() as i32);
    }
    Some(total)
}

/// Days between 1970-01-01 and the given UTC date — Howard Hinnant's
/// civil-from-days algorithm
/// (<https://howardhinnant.github.io/date_algorithms.html>).
///
/// Validates the day against the month's actual length (28/29/30/31)
/// in addition to the trivial `1..=31` envelope, so e.g. `2026-02-30`
/// returns `None` instead of silently rolling into March.
pub fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) as i64 {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn days_in_month(year: i64, month: i64) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Format a Unix-seconds value as `YYYY-MM-DDTHH:MM:SSZ` — the same
/// shape the C# build's `DateTimeOffset.UtcNow.ToString("o")` writes
/// minus the ticks-resolution fractional component. Compatible with
/// (not byte-identical to) the C# output: the `o` round-trip format
/// uses an explicit `+00:00` offset, while we emit the equivalent `Z`
/// shorthand because both round-trip through [`parse_iso8601_secs`].
pub fn iso8601_from_unix_secs(secs: i64) -> String {
    let (y, m, d, hh, mm, ss) = unix_secs_to_civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Inverse of [`days_since_epoch`] with hours / minutes / seconds.
pub fn unix_secs_to_civil(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let hh = secs_of_day / 3_600;
    let mm = (secs_of_day % 3_600) / 60;
    let ss = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hh, mm, ss)
}

/// Current wall-clock time formatted via [`iso8601_from_unix_secs`].
/// Production callers use this; tests pass fixed strings.
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601_from_unix_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_z_suffix() {
        let s = "2026-05-10T12:00:00Z";
        let secs = parse_iso8601_secs(s).unwrap();
        assert_eq!(iso8601_from_unix_secs(secs as i64), s);
    }

    #[test]
    fn z_and_offset_are_equivalent() {
        let z = parse_iso8601_secs("2026-05-10T12:00:00Z").unwrap();
        let offset = parse_iso8601_secs("2026-05-10T12:00:00+00:00").unwrap();
        assert!((z - offset).abs() < 1.0e-9);
    }

    #[test]
    fn fractional_seconds_are_added() {
        let with_z = parse_iso8601_secs("2026-05-10T12:00:00Z").unwrap();
        let with_frac = parse_iso8601_secs("2026-05-10T12:00:00.500Z").unwrap();
        assert!((with_frac - with_z - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn rejects_non_digit_fractional_to_avoid_nan() {
        // `f64::from_str` would happily turn each of these into a
        // valid f64 (NaN / +inf / 1000.0). The strict ASCII-digit
        // gate is what makes downstream `partial_cmp` sorts well-
        // defined.
        assert!(parse_iso8601_secs("2026-05-10T12:00:00.NaNZ").is_none());
        assert!(parse_iso8601_secs("2026-05-10T12:00:00.+1Z").is_none());
        assert!(parse_iso8601_secs("2026-05-10T12:00:00.1e3Z").is_none());
    }

    #[test]
    fn rejects_out_of_range_components() {
        assert!(parse_iso8601_secs("nope").is_none());
        assert!(parse_iso8601_secs("2026-13-01T00:00:00Z").is_none());
        assert!(parse_iso8601_secs("2026-02-30T00:00:00Z").is_none());
        assert!(parse_iso8601_secs("2026-04-31T00:00:00Z").is_none());
        assert!(parse_iso8601_secs("2026-05-10T24:00:00Z").is_none());
        assert!(parse_iso8601_secs("2026-05-10T12:60:00Z").is_none());
        // Leap second (sec == 60) is allowed — wallclock readings on
        // some systems hit it. sec == 61 is not.
        assert!(parse_iso8601_secs("2026-05-10T12:00:60Z").is_some());
        assert!(parse_iso8601_secs("2026-05-10T12:00:61Z").is_none());
    }

    #[test]
    fn leap_day_handling_is_calendar_aware() {
        // 2024 is a leap year (divisible by 4, not by 100).
        assert!(parse_iso8601_secs("2024-02-29T00:00:00Z").is_some());
        // 2025 is not.
        assert!(parse_iso8601_secs("2025-02-29T00:00:00Z").is_none());
        // 2000 is a leap year (divisible by 400).
        assert!(parse_iso8601_secs("2000-02-29T00:00:00Z").is_some());
        // 1900 is not (divisible by 100, not by 400).
        assert!(parse_iso8601_secs("1900-02-29T00:00:00Z").is_none());
    }

    #[test]
    fn now_is_round_trippable() {
        let now = now_iso8601();
        assert!(parse_iso8601_secs(&now).is_some(), "produced: {now}");
    }
}
