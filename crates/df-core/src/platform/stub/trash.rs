//! No trash: stands in on Windows until W4.7 (the Recycle Bin). macOS has
//! Finder's, with a restore journal (`platform/macos/trash.rs`).
//!
//! There is no home trash to find, so [`Trash::home`] and [`for_path`] refuse
//! with [`DfError::Unsupported`], and the app turns that into its refusal
//! toast: `d` says the trash is not available rather than deleting for good.
//! Every operation on a trash refuses the same way, and a listing is empty.
//!
//! [`TrashedItem`] and [`Purged`] have the fields every target shares, because
//! the undo journal and the trash view carry them whatever the platform, and
//! the two date helpers are the real ones — the `DeletionDate` format is the
//! program's own, not the freedesktop spec's, and the trash view reads it on
//! every target. What is freedesktop-only (`.trashinfo` text, `$topdir`
//! trashes, the purge stamp) has no counterpart here.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// One trashed thing, and everything needed to put it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// The trash it went into.
    pub trash_root: PathBuf,
    /// Its name in the trash.
    pub name: OsString,
    /// Where it came from, absolute.
    pub original: PathBuf,
    /// `YYYY-MM-DDThh:mm:ss`, UTC.
    pub deleted_at: String,
}

impl TrashedItem {
    /// Where the item is now. Nothing is ever trashed here, so nowhere.
    pub fn location(&self) -> PathBuf {
        PathBuf::new()
    }

    /// [`TrashedItem::location`].
    pub fn files_path(&self) -> PathBuf {
        self.location()
    }

    /// [`TrashedItem::location`]: there is no separate record.
    pub fn info_path(&self) -> PathBuf {
        self.location()
    }

    /// Never: an orphan is a freedesktop trash's file without its record.
    pub fn is_orphan(&self) -> bool {
        false
    }
}

/// A trash rooted at a directory, which this platform cannot use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    /// A trash at an explicit root: a value, so the undo journal can name the
    /// trash an item went into. Every operation on it refuses.
    pub fn at(root: impl Into<PathBuf>) -> Trash {
        Trash { root: root.into() }
    }

    /// There is no home trash on this platform (yet).
    pub fn home() -> Result<Trash> {
        Err(DfError::Unsupported("Trash"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root itself: there is no `files/` directory here.
    pub fn files_dir(&self) -> PathBuf {
        self.root.clone()
    }

    /// Refused: nothing is moved.
    pub fn trash(&self, _path: &Path, _ctx: &TaskCtx) -> Result<TrashedItem> {
        Err(DfError::Unsupported("Trash"))
    }

    /// Nothing is in it.
    pub fn list(&self) -> Result<Vec<TrashedItem>> {
        Ok(Vec::new())
    }

    /// Refused.
    pub fn restore(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
        restore(item, ctx)
    }

    /// Refused.
    pub fn purge(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<()> {
        purge(item, ctx)
    }
}

/// Refused: nothing was trashed here to destroy.
pub fn purge(_item: &TrashedItem, _ctx: &TaskCtx) -> Result<()> {
    Err(DfError::Unsupported("Trash"))
}

/// Refused: nothing was trashed here to put back.
pub fn restore(_item: &TrashedItem, _ctx: &TaskCtx) -> Result<PathBuf> {
    Err(DfError::Unsupported("Trash"))
}

/// Refused: there is no trash for any path.
pub fn for_path(_path: &Path) -> Result<Trash> {
    Err(DfError::Unsupported("Trash"))
}

/// Never: a mirror's extras here are deleted or kept, never trashed.
pub fn available_for(_dest: &Path) -> bool {
    false
}

/// Refused, as [`available_for`] has already said.
pub fn for_sync(_dest: &Path) -> Result<Trash> {
    Err(DfError::Unsupported("Trash"))
}

/// What a purge of old items came to: how many went, and how many would not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Purged {
    pub removed: usize,
    pub failed: usize,
    /// The first refusal, word for word, for the task panel and the toast.
    pub first_error: Option<String>,
}

/// Refused: there is nothing here to age out.
pub fn purge_expired(
    _trash: &Trash,
    _keep_days: u64,
    _now: SystemTime,
    _ctx: &TaskCtx,
) -> Result<Purged> {
    Err(DfError::Unsupported("Trash"))
}

/// Refused, as [`purge_expired`].
pub fn purge_expired_if_due(
    _trash: &Trash,
    _keep_days: u64,
    _every: Duration,
    _now: SystemTime,
    _ctx: &TaskCtx,
) -> Result<Option<Purged>> {
    Err(DfError::Unsupported("Trash"))
}

/// Never owed a purge: asked again in `every`, as a trash that does not exist
/// is on Linux.
pub fn purge_due_in(_trash: &Trash, every: Duration, _now: SystemTime) -> Option<Duration> {
    Some(every)
}

/// `YYYY-MM-DDThh:mm:ss` in UTC — the same text the Linux trash writes.
pub fn iso8601_utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since the epoch to a calendar date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DDThh:mm:ss` as a [`SystemTime`], read as UTC; `None` for
/// anything that does not parse. The same reading the Linux trash gives.
pub fn parse_deletion_date(text: &str) -> Option<SystemTime> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    if secs >= 0 {
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
    } else {
        SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    }
}

/// Howard Hinnant's `days_from_civil`, the inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = month as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    /// Every way into a trash refuses in words, and a listing is empty rather
    /// than an error, so the trash view opens on nothing.
    #[test]
    fn every_trash_operation_refuses_in_words() {
        let refusal = "Trash is not available on this platform";
        assert_eq!(Trash::home().unwrap_err().to_string(), refusal);
        assert_eq!(for_path(Path::new("a")).unwrap_err().to_string(), refusal);
        let trash = Trash::at("somewhere");
        assert_eq!(
            trash.trash(Path::new("a"), &ctx()).unwrap_err().to_string(),
            refusal
        );
        assert!(trash.list().is_ok_and(|items| items.is_empty()));
        assert!(!available_for(Path::new("a")));
        assert_eq!(
            purge_due_in(&trash, Duration::from_secs(60), SystemTime::now()),
            Some(Duration::from_secs(60))
        );
    }

    /// The date text round-trips, as it does on Linux.
    #[test]
    fn the_deletion_date_round_trips() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_788_183_907);
        assert_eq!(parse_deletion_date(&iso8601_utc(t)), Some(t));
        assert_eq!(parse_deletion_date("yesterday"), None);
    }
}
