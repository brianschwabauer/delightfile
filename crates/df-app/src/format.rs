//! The right-hand column of a row: whatever the current linemode says
//! (PLAN §4.1's `m` chord).
//!
//! All of it is a pure function of an [`Entry`] and a [`LineMode`], because the
//! column is drawn for every visible row on every frame and none of it may go
//! back to the filesystem — [`Entry`] was stat'd once at scan time and carries
//! everything these need.
//!
//! The one impure corner is the clock: turning a `SystemTime` into the year and
//! month a person lives in needs the machine's timezone, and the timezone lives
//! behind libc. That call is isolated to [`civil_local`]; the formatting itself
//! is [`stamp`], which is pure and tested.

use std::time::{SystemTime, UNIX_EPOCH};

use df_core::config::LineMode;
use df_core::fs::Entry;

/// What a row's second column says.
pub fn linemode_text(entry: &Entry, mode: LineMode) -> String {
    match mode {
        LineMode::Size => size_text(entry),
        LineMode::Permissions => entry.permissions_string(),
        // A hole is normal here, not an error: `btime` is absent on ext4
        // without a big enough inode and on most network mounts (see
        // [`Entry::btime`]), so the column says "nothing to show" rather than
        // going blank and looking like a rendering bug.
        LineMode::Mtime => time_text(entry.mtime),
        LineMode::Btime => time_text(entry.btime),
        LineMode::Owner => entry.owner_label(),
        LineMode::None => String::new(),
    }
}

fn time_text(time: Option<SystemTime>) -> String {
    time.and_then(local_stamp)
        .unwrap_or_else(|| UNKNOWN_SIZE.to_string())
}

/// A directory's size, which is not known. An em dash rather than `0 B`:
/// [`Entry::len`] is deliberately zero for directories until the recursive walk
/// of PLAN §7.3 exists, and printing that zero would be a lie a person can act
/// on, while a dash is visibly "nothing to say here".
const UNKNOWN_SIZE: &str = "—";

fn size_text(entry: &Entry) -> String {
    if entry.is_dir() {
        return UNKNOWN_SIZE.to_string();
    }
    human_size(entry.len)
}

/// Bytes, the way a file manager says them.
///
/// Binary multiples (1024), because this measures what a directory *costs* on
/// disk and that is the unit every other tool on the machine — `ls -lh`, `du
/// -h`, yazi itself — reports. One decimal place past the byte range: two is
/// noise at a glance and none loses the difference between a 1.2 MB and a
/// 1.9 MB photo, which is exactly the comparison the column is scanned for.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    const STEP: f64 = 1024.0;
    if bytes < STEP as u64 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= STEP && unit + 1 < UNITS.len() {
        value /= STEP;
        unit += 1;
    }
    // Rounding can push 1023.97 KB to "1024.0 KB", which is a number nobody
    // writes; carry it into the next unit instead.
    if value >= STEP - 0.05 && unit + 1 < UNITS.len() {
        value /= STEP;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `25-08-31 14:22` — the format yazi's linemode uses, kept for the reason
/// every default here is kept: the column has to look like the one it replaces.
///
/// Two-digit year because the column is scanned, not read: the day and the time
/// are what distinguish this morning's downloads from last week's, and a
/// four-digit year buys two characters of width to say "still the 2000s".
pub fn stamp(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    format!(
        "{:02}-{:02}-{:02} {:02}:{:02}",
        year.rem_euclid(100),
        month,
        day,
        hour,
        minute
    )
}

/// A timestamp for the spot panel (PLAN §6): the **full** year, and a dash when
/// there is nothing to say.
///
/// The list's column is two characters of year because it is scanned in a
/// column six times a second; the spot panel is read once, deliberately, and is
/// where "was this 2015 or 2025" gets answered.
pub fn long_stamp(time: Option<SystemTime>) -> String {
    let Some((year, month, day, hour, minute)) = time.and_then(civil_local) else {
        return UNKNOWN_SIZE.to_string();
    };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

fn local_stamp(time: SystemTime) -> Option<String> {
    let (year, month, day, hour, minute) = civil_local(time)?;
    Some(stamp(year, month, day, hour, minute))
}

/// A `SystemTime` in the machine's local timezone, as `(year, month, day, hour,
/// minute)`.
///
/// `localtime_r` rather than a hand-rolled calendar: the calendar is the easy
/// half, and the hard half — which offset applied on this date, in this zone,
/// under this year's DST rules — is a copy of `/usr/share/zoneinfo` that this
/// program has no business carrying. libc is already in the workspace (df-core
/// calls inotify through it) and this is the same kind of call.
#[allow(unsafe_code)]
fn civil_local(time: SystemTime) -> Option<(i32, u32, u32, u32, u32)> {
    // Times before 1970 are legal on a filesystem (an archive with a bogus
    // stamp, a clock that was wrong) and `duration_since` refuses them, so the
    // sign is recovered rather than dropped.
    let secs = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).ok()?,
        Err(e) => -i64::try_from(e.duration().as_secs()).ok()?,
    };
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` and `tm` are owned locals of the right types, and
    // `localtime_r` writes only into `tm`. The `_r` form is the reentrant one,
    // so there is no shared static to race a worker thread over.
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return None;
    }
    Some((
        tm.tm_year + 1900,
        (tm.tm_mon + 1) as u32,
        tm.tm_mday as u32,
        tm.tm_hour as u32,
        tm.tm_min as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_sizes_are_exact_bytes() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1), "1 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn larger_sizes_carry_one_decimal_and_binary_units() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 1024 * 3 / 2), "1.5 GB");
        assert_eq!(human_size(1024u64.pow(5)), "1.0 PB");
        // The top unit does not run out: an absurd number stays in PB rather
        // than indexing past the table.
        assert!(human_size(u64::MAX).ends_with(" PB"));
    }

    /// The rounding corner: a shade under the next unit must not print a
    /// four-digit mantissa.
    #[test]
    fn a_size_that_rounds_up_carries_into_the_next_unit() {
        assert_eq!(human_size(1024 * 1024 - 1), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 1024 - 1), "1.0 GB");
    }

    #[test]
    fn timestamps_are_two_digit_year_and_zero_padded() {
        assert_eq!(stamp(2025, 8, 31, 14, 22), "25-08-31 14:22");
        assert_eq!(stamp(2001, 1, 2, 3, 4), "01-01-02 03:04");
        // A year before 2000 still gives two digits rather than a negative.
        assert_eq!(stamp(1999, 12, 31, 23, 59), "99-12-31 23:59");
    }

    /// The one thing that can be asserted about the timezone call without
    /// pinning the machine's zone: the epoch is somewhere in 1969–1970.
    #[test]
    fn the_epoch_lands_in_the_right_year() {
        let (year, ..) = civil_local(UNIX_EPOCH).expect("localtime_r");
        assert!((1969..=1970).contains(&year), "got {year}");
    }
}
