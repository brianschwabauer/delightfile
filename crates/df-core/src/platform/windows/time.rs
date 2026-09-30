//! The local wall clock, from Windows' own time calls: the seconds become a
//! `FILETIME` (100-nanosecond ticks since 1601), `FileTimeToSystemTime` spells
//! it as a UTC calendar date, and `SystemTimeToTzSpecificLocalTime` moves that
//! to this machine's zone. The C runtime's `localtime_s` would do it in one
//! call, but refuses any time before 1970, and a scanned photo or an old
//! archive's member can carry one; the `FILETIME` covers 1601 onwards.
//!
//! The `unsafe` is those two calls, each on locals of the right types that
//! outlive it, read only when the call says it succeeded — the same rules as
//! the other islands, with no handle to own.

#![allow(unsafe_code)] // FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime on locals

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

use crate::rename::facts::Civil;

/// Seconds from 1601-01-01 to the Unix epoch.
const EPOCH_1601: i64 = 11_644_473_600;

/// `FILETIME` ticks per second.
const TICKS: i64 = 10_000_000;

/// Seconds since the epoch (negative before it) as a civil date-time on this
/// machine's local clock. `None` only for a time before 1601 or past what a
/// `FILETIME` holds, or if Windows refuses the conversion.
pub fn local_civil(secs: i64) -> Option<Civil> {
    let ticks = filetime_ticks(secs)?;
    let file_time = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = empty();
    // SAFETY: both arguments are locals of the types the call takes, and it
    // writes only `utc`.
    if unsafe { FileTimeToSystemTime(&file_time, &mut utc) } == 0 {
        return None;
    }
    let mut local = empty();
    // SAFETY: a null zone means the one this machine is set to; `utc` is
    // read and `local` written, both locals that outlive the call.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return None;
    }
    Some(Civil {
        year: i32::from(local.wYear),
        month: u32::from(local.wMonth),
        day: u32::from(local.wDay),
        hour: u32::from(local.wHour),
        minute: u32::from(local.wMinute),
        second: u32::from(local.wSecond).min(59),
    })
}

/// The `FILETIME` of `secs`: 100-nanosecond ticks since 1601, which must be
/// neither negative nor past `i64::MAX` (the most `FileTimeToSystemTime`
/// takes).
fn filetime_ticks(secs: i64) -> Option<u64> {
    let ticks = secs.checked_add(EPOCH_1601)?.checked_mul(TICKS)?;
    u64::try_from(ticks).ok()
}

fn empty() -> SYSTEMTIME {
    SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filetime_counts_from_1601() {
        assert_eq!(filetime_ticks(-EPOCH_1601), Some(0));
        assert_eq!(filetime_ticks(0), Some(116_444_736_000_000_000));
        assert_eq!(filetime_ticks(-EPOCH_1601 - 1), None, "before 1601");
        assert_eq!(filetime_ticks(i64::MAX), None);
    }

    /// 1900 is a date here, where `localtime_s` had none; the zone moves it
    /// by hours at most, so the day stays within one of New Year's.
    #[test]
    fn a_time_long_before_1970_is_a_date() {
        // 1900-01-01T12:00:00Z.
        let civil = local_civil(-2_208_945_600).expect("a date in 1900");
        assert!(
            (civil.year, civil.month, civil.day) == (1900, 1, 1)
                || (civil.year, civil.month, civil.day) == (1899, 12, 31)
                || (civil.year, civil.month, civil.day) == (1900, 1, 2),
            "{civil:?}"
        );
    }
}
