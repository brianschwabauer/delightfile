//! RFC 3339 timestamps — what rclone writes a file's `ModTime` as — to Unix
//! seconds.
//!
//! One shape: `2026-09-25T10:19:42.038713743-05:00`, the date, a `T`, the time,
//! an optional fraction of any length, and either `Z` or a `±hh:mm` offset.
//! The lower-case `t` and `z` and a space in place of the `T` are accepted too,
//! because RFC 3339 §5.6 allows the first two and its note allows the third.
//!
//! The answer is **whole seconds, rounded down**, because that is the
//! resolution a row's date is kept at ([`super::Attrs::mtime`]) and the one
//! SFTP delivers; the fraction is validated and dropped.
//!
//! A value that is not a timestamp is `None`, never an error: a listing must
//! not fail because one object's date is odd, and a row with no date is what
//! every other backend already shows for a file whose date is unknown.

/// Seconds since the Unix epoch, or `None` if `text` is not an RFC 3339
/// date-time.
pub fn parse(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    // The fixed-width part: `YYYY-MM-DDTHH:MM:SS`, nineteen bytes.
    if b.len() < 20 {
        return None;
    }
    let year = digits(b, 0, 4)?;
    let month = digits(b, 5, 2)?;
    let day = digits(b, 8, 2)?;
    let hour = digits(b, 11, 2)?;
    let minute = digits(b, 14, 2)?;
    let second = digits(b, 17, 2)?;
    if b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    // 60 is a leap second, which RFC 3339 permits and which counts, like
    // every clock that has to show one, as the next minute's first second.
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut at = 19;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == start {
            return None;
        }
    }

    let offset = match b.get(at)? {
        b'Z' | b'z' => {
            at += 1;
            0
        }
        sign @ (b'+' | b'-') => {
            if b.len() < at + 6 || b[at + 3] != b':' {
                return None;
            }
            let hours = digits(b, at + 1, 2)?;
            let minutes = digits(b, at + 4, 2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            at += 6;
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'-' {
                -seconds
            } else {
                seconds
            }
        }
        _ => return None,
    };
    if at != b.len() {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// `len` ASCII digits starting at `at`, as a number.
fn digits(b: &[u8], at: usize, len: usize) -> Option<i64> {
    let slice = b.get(at..at + len)?;
    slice.iter().try_fold(0i64, |n, &d| {
        d.is_ascii_digit().then(|| n * 10 + i64::from(d - b'0'))
    })
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`: shift the year to start in March so the
/// leap day is the last day of the year, count whole 400-year eras, then the
/// day within the era. Exact for every date, with no table and no loop.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let shifted_month = (month + 9) % 12; // March = 0
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rclones_own_format_parses() {
        // 2026-09-25T15:19:42Z, written with rclone's local offset.
        assert_eq!(
            parse("2026-09-25T10:19:42.038713743-05:00"),
            Some(1_790_349_582)
        );
        assert_eq!(parse("2026-09-25T15:19:42Z"), Some(1_790_349_582));
        assert_eq!(parse("2026-09-25T20:49:42+05:30"), Some(1_790_349_582));
    }

    #[test]
    fn the_epoch_and_its_neighbours() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("1970-01-01T00:00:01.999Z"), Some(1), "rounded down");
        assert_eq!(parse("1969-12-31T23:59:59Z"), Some(-1));
        // Go's zero time, which rclone writes when a backend has no date.
        assert_eq!(parse("0001-01-01T00:00:00Z"), Some(-62_135_596_800));
    }

    #[test]
    fn leap_years_and_leap_seconds() {
        assert_eq!(parse("2024-02-29T00:00:00Z"), Some(1_709_164_800));
        assert_eq!(parse("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(
            parse("2023-02-29T00:00:00Z"),
            None,
            "2023 is not a leap year"
        );
        assert_eq!(parse("1900-02-29T00:00:00Z"), None, "nor is 1900");
        assert_eq!(
            parse("2016-12-31T23:59:60Z"),
            parse("2017-01-01T00:00:00Z"),
            "a leap second is the next minute's first"
        );
    }

    #[test]
    fn the_permitted_variants_are_accepted() {
        let want = parse("2026-01-02T03:04:05Z");
        assert!(want.is_some());
        assert_eq!(parse("2026-01-02t03:04:05z"), want);
        assert_eq!(parse("2026-01-02 03:04:05Z"), want);
        assert_eq!(parse("2026-01-02T03:04:05.5Z"), want);
        assert_eq!(parse("2026-01-02T03:04:05+00:00"), want);
        assert_eq!(parse("2026-01-02T03:04:05-00:00"), want);
    }

    #[test]
    fn what_is_not_a_timestamp_is_none() {
        for text in [
            "",
            "2026-09-25",
            "2026-09-25T10:19:42",
            "2026-09-25T10:19:42.Z",
            "2026-09-25T10:19:42.5",
            "2026-09-25T10:19:42+0500",
            "2026-09-25T10:19:42+05",
            "2026-09-25T10:19:42+24:00",
            "2026-09-25T10:19:42Zjunk",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-09-25T24:00:00Z",
            "2026-09-25T10:60:00Z",
            "2026-09-25T10:19:61Z",
            "2026/09/25T10:19:42Z",
            "2026-09-25X10:19:42Z",
            "2026-9-25T10:19:42Z",
            "２０２６-09-25T10:19:42Z",
            "not a date at all",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }
}
