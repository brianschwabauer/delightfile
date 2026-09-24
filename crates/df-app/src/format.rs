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

/// Nothing to say here. An em dash rather than `0 B` or a blank: a zero is a
/// lie a person can act on, and a blank column looks like a rendering fault.
///
/// A directory wears it only until [`crate::folders`] has something — a child
/// count within a frame or two, a recursive size a moment later. It is still
/// the honest answer for the seconds before that, for a directory that could
/// not be read, and for a timestamp the C library would not convert.
const UNKNOWN_SIZE: &str = "—";

fn size_text(entry: &Entry) -> String {
    if entry.is_dir() {
        return UNKNOWN_SIZE.to_string();
    }
    human_size(entry.len)
}

/// The size column's text for a **directory**, from whatever the walk has said
/// so far (PLAN §7.3). `None` means "nothing yet", and the caller draws its em
/// dash.
///
/// Three answers, best first:
///
/// - a settled size — `4.2 MB`, the real recursive total;
/// - a running size — `~4.2 MB`, where the tilde means *still counting, and it
///   will only go up*. The mark is on the **left** because that is where the
///   eye starts a right-aligned number, so a column of them reads as one state
///   rather than as a footnote per row;
/// - a child count — `12 items`, which is not a size and does not look like
///   one. The unit word is the whole point: a bare `12` in a column of `4.2 MB`
///   would read as twelve bytes. A count that hit the counting cap says
///   `10,000+ items`: the pass stopped there, and rounding "a lot" up to a
///   precise-looking number would be a figure nobody could reproduce. The
///   count is grouped like every other count a person reads, and the same way
///   [`df_core::du::MAX_COUNTED_ENTRIES`]'s own doc spells it.
pub fn folder_size_text(
    size: Option<crate::folders::Size>,
    count: Option<df_core::du::ChildCount>,
) -> Option<String> {
    if let Some(size) = size {
        let bytes = human_size(size.bytes);
        return Some(if size.settled {
            bytes
        } else {
            format!("~{bytes}")
        });
    }
    let count = count?;
    let plus = if count.capped { "+" } else { "" };
    Some(format!(
        "{}{plus} {}",
        df_core::text::grouped(count.entries),
        if count.entries == 1 && !count.capped {
            "item"
        } else {
            "items"
        }
    ))
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

/// `20260831_142233` — a stamp that is safe in a **file name**.
///
/// The clipboard's own paste target (PLAN §7.4: an image on the clipboard is
/// saved as `clipboard_<stamp>.png`). No colons, no spaces and no dashes to be
/// mistaken for an option: this string ends up on a command line sooner or
/// later. Local time, so the file sorts next to whatever else was made this
/// afternoon, and seconds, because two pastes a minute apart is not a rare
/// thing to do.
pub fn file_stamp(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() % 60)
        .unwrap_or(0);
    match civil_local(time) {
        Some((year, month, day, hour, minute)) => {
            format!("{year:04}{month:02}{day:02}_{hour:02}{minute:02}{seconds:02}")
        }
        // A clock the C library cannot make sense of still has to produce a
        // unique name; the epoch second is one.
        None => format!(
            "{}",
            time.duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ),
    }
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

    /// The three things a directory row can say, and the order they are said
    /// in. This is the whole of PLAN §7.3's size column as a pure function.
    #[test]
    fn a_folder_says_the_best_thing_it_knows() {
        use crate::folders::Size;
        let settled = |bytes| {
            Some(Size {
                bytes,
                settled: true,
            })
        };
        let counting = |bytes| {
            Some(Size {
                bytes,
                settled: false,
            })
        };
        let count = |entries| {
            Some(df_core::du::ChildCount {
                entries,
                capped: false,
            })
        };
        let capped = |entries| {
            Some(df_core::du::ChildCount {
                entries,
                capped: true,
            })
        };

        assert_eq!(folder_size_text(None, None), None, "the em dash stands");
        assert_eq!(
            folder_size_text(None, count(12)).as_deref(),
            Some("12 items")
        );
        // A count is a count, not a size: singular reads as English and an
        // empty folder says so rather than showing a dash.
        assert_eq!(folder_size_text(None, count(1)).as_deref(), Some("1 item"));
        assert_eq!(folder_size_text(None, count(0)).as_deref(), Some("0 items"));
        // A count that stopped at the cap says so rather than claiming to be
        // the answer.
        assert_eq!(
            folder_size_text(None, capped(10_000)).as_deref(),
            Some("10,000+ items")
        );

        // A size outranks a count the moment there is one, tilde and all.
        assert_eq!(
            folder_size_text(counting(1536), count(12)).as_deref(),
            Some("~1.5 KB")
        );
        assert_eq!(
            folder_size_text(settled(1536), count(12)).as_deref(),
            Some("1.5 KB")
        );
        // A settled empty directory is `0 B`, which is true — the dash was
        // only ever "we do not know".
        assert_eq!(folder_size_text(settled(0), None).as_deref(), Some("0 B"));
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
