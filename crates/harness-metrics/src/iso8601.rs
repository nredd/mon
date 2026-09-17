//! A minimal `YYYY-MM-DDTHH:MM:SS[.sss]Z` parser, shared by every backend.
//!
//! Every harness this crate reads timestamps every billable record with a fixed-width,
//! always-UTC ISO-8601 instant. Hand-parsed rather than pulled in through a date library:
//! it is a few field reads and a days-since-epoch calculation, and it keeps a crate whose
//! whole point is having no dependency on `mon` from growing one on `chrono` for one field.

/// Parse `YYYY-MM-DDTHH:MM:SS[.sss]Z` into Unix epoch milliseconds.
///
/// Only this exact shape is accepted. Anything else returns `None` and the caller drops
/// the record rather than guessing a time for it. Offsets other than `Z` are deliberately
/// unsupported -- silently treating `+02:00` as UTC would put records in the wrong bucket,
/// which is worse than losing them.
#[must_use]
pub(crate) fn parse_ms(stamp: &str) -> Option<u64> {
    let bytes = stamp.as_bytes();

    // The shortest accepted form is `YYYY-MM-DDTHH:MM:SSZ`.
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }

    if bytes[13] != b':' || bytes[16] != b':' || !stamp.ends_with('Z') {
        return None;
    }

    let field = |from: usize, to: usize| stamp.get(from..to)?.parse::<u64>().ok();

    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);

    // The fractional part is optional and of unspecified length; take up to three digits
    // and pad, so both `.9` and `.919` mean what they say.
    let millis = match bytes.get(19) {
        Some(b'.') => {
            let digits: String = stamp[20..]
                .chars()
                .take_while(char::is_ascii_digit)
                .take(3)
                .collect();

            // At most three digits were taken, so this cannot underflow.
            let missing = 3 - u32::try_from(digits.len()).ok()?;
            digits.parse::<u64>().ok()? * 10u64.pow(missing)
        }
        _ => 0,
    };

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day)?;

    Some(
        days.saturating_mul(86_400_000)
            .saturating_add(hour * 3_600_000)
            .saturating_add(minute * 60_000)
            .saturating_add(second * 1_000)
            .saturating_add(millis),
    )
}

/// Days from 1970-01-01 to a civil date, by Howard Hinnant's `days_from_civil`.
///
/// The shift-the-year-to-March trick is what makes leap days fall out of the arithmetic
/// instead of needing a special case: with March as month zero, the leap day lands at the
/// end of the year where it cannot disturb the month-length series.
fn days_from_civil(year: u64, month: u64, day: u64) -> Option<u64> {
    // Before the epoch there is nothing to attribute, and the unsigned arithmetic below
    // would wrap rather than go negative.
    if year < 1970 {
        return None;
    }

    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;

    let month_shift = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_shift + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    // 719468 is the day count from 0000-03-01 to 1970-01-01.
    Some(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_real_timestamp_parses_to_epoch_millis() {
        // Taken verbatim from a live transcript.
        assert_eq!(
            parse_ms("2026-08-24T20:26:13.919Z"),
            Some(1_787_603_173_919)
        );
    }

    #[test]
    fn the_epoch_itself_is_zero() {
        assert_eq!(parse_ms("1970-01-01T00:00:00.000Z"), Some(0));
    }

    #[test]
    fn leap_days_are_counted() {
        // The whole reason for the shift-the-year-to-March trick. A day either side of the
        // 2024 leap day must be exactly 86400000ms apart across it.
        let before = parse_ms("2024-02-28T00:00:00Z").expect("valid");
        let leap = parse_ms("2024-02-29T00:00:00Z").expect("valid");
        let after = parse_ms("2024-03-01T00:00:00Z").expect("valid");

        assert_eq!(leap - before, 86_400_000);
        assert_eq!(after - leap, 86_400_000);
    }

    #[test]
    fn a_century_that_is_not_a_leap_year_is_handled() {
        // 1900 is not a leap year but 2000 is, which is the case a naive `% 4` gets wrong.
        let before = parse_ms("2000-02-28T00:00:00Z").expect("valid");
        let after = parse_ms("2000-03-01T00:00:00Z").expect("valid");

        assert_eq!(after - before, 2 * 86_400_000, "2000 had a leap day");
    }

    #[test]
    fn a_short_fraction_is_padded_rather_than_misread() {
        // `.9` is nine hundred milliseconds, not nine.
        assert_eq!(
            parse_ms("2026-08-24T20:26:13.9Z"),
            parse_ms("2026-08-24T20:26:13.900Z")
        );
    }

    #[test]
    fn a_missing_fraction_is_fine() {
        assert_eq!(
            parse_ms("2026-08-24T20:26:13Z"),
            parse_ms("2026-08-24T20:26:13.000Z")
        );
    }

    #[test]
    fn a_non_utc_offset_is_refused_rather_than_silently_misread() {
        // Treating `+02:00` as UTC would put records two hours into the wrong bucket, which
        // is worse than losing them.
        assert_eq!(parse_ms("2026-08-24T20:26:13.919+02:00"), None);
    }

    #[test]
    fn junk_timestamps_yield_nothing_rather_than_panicking() {
        for stamp in [
            "",
            "not a date",
            "2026-08-24",
            "2026-13-01T00:00:00Z",
            "2026-08-24T25:00:00Z",
            "1969-12-31T23:59:59Z",
        ] {
            assert_eq!(parse_ms(stamp), None, "{stamp:?} must not parse");
        }
    }
}
