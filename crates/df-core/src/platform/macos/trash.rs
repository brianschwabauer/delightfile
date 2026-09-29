//! The macOS trash: Finder's, through `NSFileManager`, with a journal of our
//! own so that `u` can put things back.
//!
//! `d` must always be reversible (PLAN §5). On Linux the trash itself keeps
//! the record — a `.trashinfo` beside every file — and a restore reads it. The
//! Mac's Trash keeps none a program can read: `trashItemAtURL:
//! resultingItemURL:error:` moves an item into the Trash of the volume it is
//! on (`~/.Trash` on the startup volume, `.Trashes/<uid>` at the root of any
//! other), under a new name if its own is taken, and says where it put it,
//! and that is all. Finder's "Put Back" works from Finder's private records.
//!
//! So delightfile writes its own: a **journal**, one line per item it has
//! trashed, `<deleted at>\t<original path>\t<where it is now>` (the paths
//! escaped as the state file escapes them), in delightfile's state directory
//! beside the `state` file. A [`Trash`] *is* that journal — its root is the
//! journal's path — and a [`TrashedItem`] names the journal it is in (its
//! `trash_root`) and its name in the Trash folder; where it is now is read
//! back from its line ([`TrashedItem::location`]), so the undo journal and
//! the trash view carry the same four fields they carry on Linux.
//!
//! What that means for the person:
//!
//! - The trash view lists what delightfile trashed and nothing else. Finder's
//!   Trash may hold more; those items are Finder's to put back.
//! - A line whose item is no longer where it says — emptied in Finder, put
//!   back by hand — is not listed, and is dropped the next time the journal is
//!   rewritten.
//! - Restoring renames the item back (copying across volumes if it has to)
//!   and removes its line; destroying one ([`purge`]) removes the line first,
//!   then the item, the order the Linux trash keeps for the same reason: an
//!   interrupted purge must not leave a record that promises a whole item.
//!
//! Several windows are several processes, so every change to the journal is
//! made under an exclusive lock on a file beside it (`flock`, through
//! `File::lock`): an append is one write of a whole line, and a rewrite is a
//! temporary file renamed over the journal.
//!
//! Emptying old items (`[mgr] trash_keep_days`) is not done here yet:
//! [`purge_due_in`] is never due (M2.34).

// Foundation's methods are `unsafe fn` in objc2-foundation 0.2: every call
// here is one of three (`defaultManager`, a file URL from a path's bytes,
// `trashItemAtURL`), each with the SAFETY note that says what it is trusted
// with, in [`move_to_trash`] and nowhere else.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use objc2_foundation::{NSFileManager, NSURL};

use crate::ops::{exists, normalize};
use crate::platform::errno;
use crate::state::{escape, unescape};
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// The journal's file name, in delightfile's state directory.
const JOURNAL: &str = "trash-journal";

/// One trashed thing, and everything needed to put it back.
///
/// The fields are Linux's, meaning what they can on a Mac: the trash it went
/// into is the journal that records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// The journal it is recorded in: the [`Trash`] it went into.
    pub trash_root: PathBuf,
    /// Its name in the Trash folder — its own, or the one Finder's rules gave
    /// it when that was taken.
    pub name: OsString,
    /// Where it came from, absolute.
    pub original: PathBuf,
    /// `YYYY-MM-DDThh:mm:ss`, UTC.
    pub deleted_at: String,
}

impl TrashedItem {
    /// Where the item is now, as its journal line says: empty when the line is
    /// gone (the item restored, destroyed, or the journal lost), which every
    /// caller reads as "no longer in the trash".
    pub fn location(&self) -> PathBuf {
        recorded(&self.trash_root)
            .iter()
            .find(|line| self.is(line))
            .map(|line| line.location.clone())
            .unwrap_or_default()
    }

    /// [`TrashedItem::location`].
    pub fn files_path(&self) -> PathBuf {
        self.location()
    }

    /// [`TrashedItem::location`]: the record is a line of the journal, not a
    /// file of its own.
    pub fn info_path(&self) -> PathBuf {
        self.location()
    }

    /// Never, in practice: every line records where its item came from.
    pub fn is_orphan(&self) -> bool {
        self.original.as_os_str().is_empty()
    }

    /// Whether `line` is this item's.
    fn is(&self, line: &Line) -> bool {
        line.deleted_at == self.deleted_at
            && line.original == self.original
            && line.location.file_name() == Some(self.name.as_os_str())
    }
}

/// A trash: the journal at its root, and the items it records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    /// The trash whose journal is the file at `root`: a value, so the undo
    /// journal can name the trash an item went into, and a test can keep its
    /// own journal. The items themselves go where macOS puts them.
    pub fn at(root: impl Into<PathBuf>) -> Trash {
        Trash { root: root.into() }
    }

    /// The user's trash: the journal in delightfile's state directory.
    pub fn home() -> Result<Trash> {
        let dir = crate::platform::dirs::state_dir()
            .ok_or_else(|| DfError::Op("$HOME is not set: no trash journal".to_string()))?;
        Ok(Trash::at(dir.join("delightfile").join(JOURNAL)))
    }

    /// The journal's path.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The journal's path too: there is no one folder the items are in.
    pub fn files_dir(&self) -> PathBuf {
        self.root.clone()
    }

    /// Move `path` to the Trash of its volume and record where it went.
    ///
    /// If the record cannot be written the item is moved back, and the error
    /// is the journal's: a `d` that `u` could not undo is not one to make.
    pub fn trash(&self, path: &Path, _ctx: &TaskCtx) -> Result<TrashedItem> {
        if !exists(path) {
            return Err(DfError::io(
                path,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            ));
        }
        // The same rails as a permanent delete: trashing `$HOME` is no more
        // sensible than deleting it.
        crate::ops::delete::check_deletable_here(path)?;

        let original = normalize(path);
        let location = move_to_trash(&original)?;
        let deleted_at = iso8601_utc(SystemTime::now());
        let line = Line {
            deleted_at: deleted_at.clone(),
            original: original.clone(),
            location: location.clone(),
        };
        if let Err(e) = append(&self.root, &line) {
            return Err(match std::fs::rename(&location, &original) {
                Ok(()) => DfError::io(&self.root, e),
                Err(back) => DfError::Op(format!(
                    "{} is in the Trash as {}, but delightfile could not record it ({e}) \
                     nor put it back ({back})",
                    original.display(),
                    location.display()
                )),
            });
        }
        let name = location.file_name().unwrap_or_default().to_os_string();
        Ok(TrashedItem {
            trash_root: self.root.clone(),
            name,
            original,
            deleted_at,
        })
    }

    /// Every item the journal records that is still where it says, for the
    /// trash view and the tests. Lines that cannot be read are logged and
    /// skipped.
    pub fn list(&self) -> Result<Vec<TrashedItem>> {
        let mut out: Vec<TrashedItem> = recorded(&self.root)
            .iter()
            .filter(|line| std::fs::symlink_metadata(&line.location).is_ok())
            .map(|line| TrashedItem {
                trash_root: self.root.clone(),
                name: line.location.file_name().unwrap_or_default().to_os_string(),
                original: line.original.clone(),
                deleted_at: line.deleted_at.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Put an item back where it came from.
    pub fn restore(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
        restore(item, ctx)
    }

    /// Destroy one item for good — the trash view's `D`.
    pub fn purge(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<()> {
        purge(item, ctx)
    }
}

/// Destroy a trashed item for good: its line, then the item.
///
/// The line goes first for the Linux trash's reason (see its `purge`): a
/// cancelled delete of a folder leaves part of it in the Trash, which must not
/// sit behind a record that says the whole of it is there to put back. What a
/// cancel leaves is in Finder's Trash, not listed here.
pub fn purge(item: &TrashedItem, ctx: &TaskCtx) -> Result<()> {
    let location = item.location();
    forget(&item.trash_root, item)?;
    if !location.as_os_str().is_empty() && exists(&location) {
        crate::ops::delete::delete_permanent(&location, ctx)?;
    }
    Ok(())
}

/// Whether a recorded original is somewhere a restore may write: absolute, and
/// with no `..` for `rename` to resolve somewhere else. The journal is a file
/// in the user's state directory, and what it says is still input.
fn restorable_destination(original: &Path) -> bool {
    original.is_absolute()
        && !original
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Put a trashed item back where it came from — the inverse `u` runs.
///
/// Refuses if something has taken the original name in the meantime: undo may
/// never overwrite newer work (PLAN §5).
pub fn restore(item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
    if !restorable_destination(&item.original) {
        return Err(DfError::Op(format!(
            "{} is not a path a restore may write to",
            item.original.display()
        )));
    }
    let src = item.location();
    if src.as_os_str().is_empty() || !exists(&src) {
        return Err(DfError::Op(format!(
            "{} is no longer in the trash",
            item.original.display()
        )));
    }
    if exists(&item.original) {
        return Err(DfError::Op(format!(
            "{} exists again: refusing to overwrite it with the trashed copy",
            item.original.display()
        )));
    }
    let parent = item
        .original
        .parent()
        .ok_or_else(|| DfError::Op(format!("{} has no parent", item.original.display())))?;
    if !exists(parent) {
        return Err(DfError::Op(format!(
            "{} is gone, so {} cannot go back into it",
            parent.display(),
            item.original.display()
        )));
    }

    match std::fs::rename(&src, &item.original) {
        Ok(()) => {}
        Err(e) if errno::is_cross_device(&e) => {
            crate::ops::copy::move_cross_device(&src, &item.original, ctx)?;
        }
        Err(e) => return Err(DfError::io(&src, e)),
    }

    if let Err(e) = forget(&item.trash_root, item) {
        // The item is back, which is what was asked for; a stale line is
        // dropped by the next rewrite, since its item is not there any more.
        log::warn!(
            "restored {} but could not update {}: {e}",
            item.original.display(),
            item.trash_root.display()
        );
    }
    Ok(item.original.clone())
}

/// The trash for `path`: the user's, for every path. macOS chooses the
/// volume's own Trash when the item is moved.
pub fn for_path(_path: &Path) -> Result<Trash> {
    Trash::home()
}

/// Whether a mirror's extras under `dest` can go to a trash: yes on a local
/// volume, which macOS gives a Trash of its own; not on a network one.
pub fn available_for(dest: &Path) -> bool {
    Trash::home().is_ok() && !crate::platform::fs::is_remote(dest)
}

/// The trash a mirror's extras under `dest` go into, refused where
/// [`available_for`] says there is none.
pub fn for_sync(dest: &Path) -> Result<Trash> {
    if !available_for(dest) {
        return Err(DfError::Op(format!("no trash for {}", dest.display())));
    }
    Trash::home()
}

/// What a purge of old items came to: how many went, and how many would not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Purged {
    pub removed: usize,
    pub failed: usize,
    /// The first refusal, word for word, for the task panel and the toast.
    pub first_error: Option<String>,
}

/// Refused: old items are not emptied on macOS yet (M2.34).
pub fn purge_expired(
    _trash: &Trash,
    _keep_days: u64,
    _now: SystemTime,
    _ctx: &TaskCtx,
) -> Result<Purged> {
    Err(DfError::Unsupported("Emptying old trash"))
}

/// Refused, as [`purge_expired`].
pub fn purge_expired_if_due(
    _trash: &Trash,
    _keep_days: u64,
    _every: Duration,
    _now: SystemTime,
    _ctx: &TaskCtx,
) -> Result<Option<Purged>> {
    Err(DfError::Unsupported("Emptying old trash"))
}

/// Never owed a purge until M2.34: asked again in `every`.
pub fn purge_due_in(_trash: &Trash, every: Duration, _now: SystemTime) -> Option<Duration> {
    Some(every)
}

// ── Foundation ──────────────────────────────────────────────────────────────

/// Move `path` to the Trash of its volume with `NSFileManager`, and answer
/// where it went. A link is moved as itself, never followed.
fn move_to_trash(path: &Path) -> Result<PathBuf> {
    let is_dir = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir());
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| DfError::Op(format!("{}: path contains a NUL byte", path.display())))?;
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: `defaultManager` takes nothing and is safe from any thread.
        // The URL is made from a NUL-terminated string that outlives the call,
        // which Foundation copies. `trashItemAtURL` is handed a live URL and a
        // live out-slot, and `fileSystemRepresentation` points into the result
        // URL, which is retained in `moved` while its bytes are copied out.
        unsafe {
            let manager = NSFileManager::defaultManager();
            let url = NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                NonNull::new_unchecked(c_path.as_ptr().cast_mut()),
                is_dir,
                None,
            );
            let mut moved: Option<objc2::rc::Retained<NSURL>> = None;
            manager
                .trashItemAtURL_resultingItemURL_error(&url, Some(&mut moved))
                .map_err(|e| DfError::Op(format!("{}: {e}", path.display())))?;
            let moved = moved.ok_or_else(|| {
                DfError::Op(format!(
                    "{}: the Trash did not say where it went",
                    path.display()
                ))
            })?;
            let bytes = CStr::from_ptr(moved.fileSystemRepresentation().as_ptr()).to_bytes();
            Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
        }
    })
}

// ── The journal ─────────────────────────────────────────────────────────────

/// One journal line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    deleted_at: String,
    original: PathBuf,
    location: PathBuf,
}

impl Line {
    fn render(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(self.deleted_at.as_bytes());
        out.push(b'\t');
        out.extend_from_slice(&escape(self.original.as_os_str().as_bytes()));
        out.push(b'\t');
        out.extend_from_slice(&escape(self.location.as_os_str().as_bytes()));
        out.push(b'\n');
        out
    }

    /// A line of the journal, or `None` for one that is not three fields that
    /// decode.
    fn parse(bytes: &[u8]) -> Option<Line> {
        let mut fields = bytes.split(|b| *b == b'\t');
        let deleted_at = std::str::from_utf8(fields.next()?).ok()?.to_string();
        let original = PathBuf::from(OsString::from_vec(unescape(fields.next()?)?));
        let location = PathBuf::from(OsString::from_vec(unescape(fields.next()?)?));
        if fields.next().is_some() || !original.is_absolute() || !location.is_absolute() {
            return None;
        }
        Some(Line {
            deleted_at,
            original,
            location,
        })
    }
}

/// The journal's lines, each parsed or, when it does not parse, as it was.
fn read(journal: &Path) -> std::io::Result<Vec<std::result::Result<Line, Vec<u8>>>> {
    let mut bytes = Vec::new();
    match File::open(journal) {
        Ok(mut file) => {
            file.read_to_end(&mut bytes)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    Ok(bytes
        .split(|b| *b == b'\n')
        .filter(|raw| !raw.is_empty())
        .map(|raw| Line::parse(raw).ok_or_else(|| raw.to_vec()))
        .collect())
}

/// The lines the journal at `journal` records, as of now.
///
/// Remembered for the last journal asked about, and read again whenever its
/// size or date says it changed: the trash view asks once per row, and each
/// row's answer is in the same file.
fn recorded(journal: &Path) -> Arc<Vec<Line>> {
    type Cached = (PathBuf, u64, Option<SystemTime>, Arc<Vec<Line>>);
    static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
    let Ok(meta) = std::fs::metadata(journal) else {
        return Arc::new(Vec::new());
    };
    let key = (meta.len(), meta.modified().ok());
    let mut cache = match CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some((path, len, modified, lines)) = cache.as_ref() {
        if path == journal && (*len, *modified) == key {
            return Arc::clone(lines);
        }
    }
    let lines: Vec<Line> = match read(journal) {
        Ok(lines) => lines
            .into_iter()
            .filter_map(|line| match line {
                Ok(line) => Some(line),
                Err(raw) => {
                    log::warn!(
                        "{}: an unreadable line: {}",
                        journal.display(),
                        String::from_utf8_lossy(&raw)
                    );
                    None
                }
            })
            .collect(),
        Err(e) => {
            log::warn!("{}: {e}", journal.display());
            Vec::new()
        }
    };
    let lines = Arc::new(lines);
    *cache = Some((journal.to_path_buf(), key.0, key.1, Arc::clone(&lines)));
    lines
}

/// Hold the journal's lock, beside it, for as long as the answer lives.
fn lock(journal: &Path) -> std::io::Result<File> {
    if let Some(dir) = journal.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = journal.as_os_str().to_os_string();
    name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))?;
    file.lock()?;
    Ok(file)
}

/// Add `line` to the journal, making the journal if it is not there.
fn append(journal: &Path, line: &Line) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let _lock = lock(journal)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(journal)?;
    file.write_all(&line.render())?;
    Ok(())
}

/// Take `item`'s line out of the journal, and with it every line whose item
/// is not where it says any more. Lines that do not parse are kept as they
/// are: they are somebody's record, even if not one this build can read.
fn forget(journal: &Path, item: &TrashedItem) -> Result<()> {
    rewrite(journal, |line| {
        !item.is(line) && std::fs::symlink_metadata(&line.location).is_ok()
    })
    .map_err(|e| DfError::io(journal, e))
}

fn rewrite(journal: &Path, keep: impl Fn(&Line) -> bool) -> std::io::Result<()> {
    let _lock = lock(journal)?;
    let lines = read(journal)?;
    let mut out = Vec::new();
    for line in &lines {
        match line {
            Ok(line) if keep(line) => out.extend_from_slice(&line.render()),
            Ok(_) => {}
            Err(raw) => {
                out.extend_from_slice(raw);
                out.push(b'\n');
            }
        }
    }
    if out.is_empty() {
        return match std::fs::remove_file(journal) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let mut temp = journal.as_os_str().to_os_string();
    temp.push(format!(".tmp.{}", std::process::id()));
    let temp = PathBuf::from(temp);
    let written = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .and_then(|mut file| file.write_all(&out))
    };
    if let Err(e) = written.and_then(|()| std::fs::rename(&temp, journal)) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    Ok(())
}

// ── Dates ───────────────────────────────────────────────────────────────────

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
    use crate::ops::fixture::TempTree;

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    /// **M2.8's proof**: a file trashed through Finder's Trash is there, and
    /// comes back, and the journal that knew about it is empty again.
    #[test]
    fn a_trashed_file_is_in_the_trash_and_comes_back() {
        let t = TempTree::new("mac-trash");
        let trash = Trash::at(t.join("journal"));
        let file = t.file("work/notes.txt", b"the contents");

        let item = trash.trash(&file, &ctx()).unwrap();
        assert!(!exists(&file));
        assert_eq!(item.trash_root, t.join("journal"));
        assert_eq!(item.original, normalize(&file));
        let location = item.location();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert!(
            location.starts_with(home.join(".Trash")),
            "{}",
            location.display()
        );
        assert_eq!(std::fs::read(&location).unwrap(), b"the contents");
        assert_eq!(trash.list().unwrap(), std::slice::from_ref(&item));

        assert_eq!(restore(&item, &ctx()).unwrap(), normalize(&file));
        assert_eq!(std::fs::read(&file).unwrap(), b"the contents");
        assert!(!exists(&location));
        assert!(trash.list().unwrap().is_empty());
        assert!(!exists(&t.join("journal")), "the journal is empty again");
    }

    /// Two trashed things of one name: the Trash names the second itself, the
    /// journal tells them apart, and each comes back to its own place.
    #[test]
    fn two_items_of_one_name_are_told_apart() {
        let t = TempTree::new("mac-trash-twins");
        let trash = Trash::at(t.join("journal"));
        let a = t.file("one/same.txt", b"one");
        let b = t.file("two/same.txt", b"two");
        let first = trash.trash(&a, &ctx()).unwrap();
        let second = trash.trash(&b, &ctx()).unwrap();
        assert_ne!(first.location(), second.location());
        assert_eq!(trash.list().unwrap().len(), 2);

        restore(&second, &ctx()).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        assert_eq!(trash.list().unwrap(), std::slice::from_ref(&first));
        let where_first = first.location();
        purge(&first, &ctx()).unwrap();
        assert!(!exists(&where_first) && trash.list().unwrap().is_empty());
        assert!(!exists(&a));
    }

    /// A broken link is trashed as itself and put back as itself.
    #[test]
    fn a_broken_link_is_trashed_without_following_it() {
        let t = TempTree::new("mac-trash-link");
        let trash = Trash::at(t.join("journal"));
        let link = t.symlink("/nowhere/at/all", "work/dangling");
        let item = trash.trash(&link, &ctx()).unwrap();
        assert!(std::fs::symlink_metadata(item.location())
            .unwrap()
            .is_symlink());
        restore(&item, &ctx()).unwrap();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("/nowhere/at/all")
        );
    }

    /// Undo may never overwrite newer work: a name taken again is refused,
    /// and the item stays where it is, recorded.
    #[test]
    fn a_restore_refuses_a_name_taken_again() {
        let t = TempTree::new("mac-trash-taken");
        let trash = Trash::at(t.join("journal"));
        let file = t.file("work/notes.txt", b"old");
        let item = trash.trash(&file, &ctx()).unwrap();
        std::fs::write(&file, b"newer").unwrap();
        let err = restore(&item, &ctx()).unwrap_err();
        assert!(err.to_string().contains("exists again"), "{err}");
        assert_eq!(std::fs::read(&file).unwrap(), b"newer");
        assert_eq!(trash.list().unwrap(), std::slice::from_ref(&item));
        purge(&item, &ctx()).unwrap();
    }

    /// The journal's lines survive every byte a path can hold, and one that
    /// is not a line is kept by a rewrite rather than lost.
    #[test]
    fn journal_lines_round_trip_and_nonsense_is_kept() {
        let line = Line {
            deleted_at: "2026-09-29T12:00:00".to_string(),
            original: PathBuf::from("/Users/me/tab\there\nnew=line \\ ünï.txt"),
            location: PathBuf::from("/Users/me/.Trash/tab\there 2.txt"),
        };
        let rendered = line.render();
        assert_eq!(rendered.iter().filter(|b| **b == b'\n').count(), 1);
        assert_eq!(
            Line::parse(&rendered[..rendered.len() - 1]),
            Some(line.clone())
        );
        assert_eq!(Line::parse(b"not\ta line"), None);
        assert_eq!(Line::parse(b"x\trelative\t/abs"), None);

        let t = TempTree::new("mac-trash-journal");
        let journal = t.join("journal");
        std::fs::write(&journal, b"garbage without tabs\n").unwrap();
        append(&journal, &line).unwrap();
        rewrite(&journal, |_| false).unwrap();
        assert_eq!(std::fs::read(&journal).unwrap(), b"garbage without tabs\n");
    }

    /// The date text round-trips, as it does on Linux.
    #[test]
    fn the_deletion_date_round_trips() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_788_183_907);
        assert_eq!(parse_deletion_date(&iso8601_utc(t)), Some(t));
        assert_eq!(parse_deletion_date("yesterday"), None);
    }
}
