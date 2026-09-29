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
//! Emptying old items (`[mgr] trash_keep_days`) works over the journal too
//! (M2.35): a line whose date is that many days old has its item destroyed,
//! once a day for all windows, through a stamp beside the journal. Only what
//! the journal records is ever aged out, so nothing Finder or anything else
//! put in the Trash is touched.

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

// ── Keeping the trash from growing for ever (`[mgr] trash_keep_days`) ───────

/// One day, in the unit a `SystemTime` is compared in.
const DAY_SECS: u64 = 86_400;

/// The items a journal kept for `keep_days` days may lose at `now`: every
/// one whose recorded date reads, and reads at least that many days ago.
///
/// Pure, for the reason Linux's is: it is the one decision that destroys
/// files nobody pointed at. Never chosen: anything when `keep_days` is `0`
/// ("never purge"), a line whose date does not read, and a date in the
/// future. Unlike Linux's there is no slack for time zones: the journal is
/// delightfile's alone and its dates are UTC, so thirty days is thirty days.
///
/// What is not in the journal is never an item here at all: Finder's own
/// things in the Trash, and anything else put there, are not delightfile's
/// to age out.
fn expired(items: &[TrashedItem], now: SystemTime, keep_days: u64) -> Vec<TrashedItem> {
    if keep_days == 0 {
        return Vec::new();
    }
    let Some(cutoff) = now.checked_sub(Duration::from_secs(keep_days.saturating_mul(DAY_SECS)))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| !item.is_orphan())
        .filter(|item| parse_deletion_date(&item.deleted_at).is_some_and(|at| at <= cutoff))
        .cloned()
        .collect()
}

/// What a purge of old items came to: how many went, and how many would not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Purged {
    pub removed: usize,
    pub failed: usize,
    /// The first refusal, word for word, for the task panel and the toast.
    pub first_error: Option<String>,
}

/// Destroy every item the journal records that [`expired`] chooses, line
/// first and then the item, as [`purge`] does one.
///
/// As on Linux, an item that will not go is counted and stepped over, the
/// first reason kept; a cancel is the only error that ends the run early.
/// Age is `now` less the recorded date, and a clock that has jumped ahead
/// purges as old everything it has passed.
pub fn purge_expired(
    trash: &Trash,
    keep_days: u64,
    now: SystemTime,
    ctx: &TaskCtx,
) -> Result<Purged> {
    let old = expired(&trash.list()?, now, keep_days);
    let mut report = Purged::default();
    for item in &old {
        ctx.checkpoint()?;
        // Asked again at the last moment: an item restored and trashed
        // again under the same name since the listing has a newer line,
        // and is not the item the listing chose.
        if !still_recorded(item) {
            continue;
        }
        match purge(item, ctx) {
            Ok(()) => report.removed += 1,
            Err(DfError::Cancelled) => return Err(DfError::Cancelled),
            Err(e) => {
                report.failed += 1;
                report.first_error.get_or_insert_with(|| e.to_string());
            }
        }
    }
    Ok(report)
}

/// Whether the journal still has `item`'s line, read from the file rather
/// than from [`recorded`]'s memory, since a purge is about to act on it.
fn still_recorded(item: &TrashedItem) -> bool {
    read(&item.trash_root).is_ok_and(|lines| {
        lines
            .iter()
            .any(|line| line.as_ref().is_ok_and(|line| item.is(line)))
    })
}

// ── Once a day per user, not per window ─────────────────────────────────────

/// The stamp beside a journal: `trash-journal.purge`. Its mtime is the last
/// purge and its being non-empty says one has happened; an `flock` on it is
/// the one purge at a time, since every window is a process of its own and
/// each keeps a daily clock (Linux's `PURGE_STAMP` says the rest). The
/// journal's own lock is another file, which a purge takes and lets go of
/// once per item it destroys.
fn stamp_of(trash: &Trash) -> PathBuf {
    let mut name = trash.root().as_os_str().to_os_string();
    name.push(".purge");
    PathBuf::from(name)
}

/// How long until a journal stamped at `stamped` is owed a purge, `every`
/// apart; `None` when it is owed one now. Never stamped is owed now, and a
/// stamp in the future waits a whole `every`.
fn purge_wait(stamped: Option<SystemTime>, every: Duration, now: SystemTime) -> Option<Duration> {
    let stamped = stamped?;
    match now.duration_since(stamped) {
        Ok(since) if since >= every => None,
        Ok(since) => Some(every - since),
        Err(_) => Some(every),
    }
}

/// When the journal was last purged, by its stamp: `None` for never.
fn read_stamp(meta: &std::fs::Metadata) -> Option<SystemTime> {
    (meta.len() > 0).then(|| meta.modified().ok()).flatten()
}

/// How long until `trash` is owed a purge, asked without taking the lock;
/// `None` is now. A journal that does not exist has nothing to purge and is
/// asked again in `every`.
pub fn purge_due_in(trash: &Trash, every: Duration, now: SystemTime) -> Option<Duration> {
    if !exists(trash.root()) {
        return Some(every);
    }
    let stamped = std::fs::metadata(stamp_of(trash))
        .ok()
        .and_then(|meta| read_stamp(&meta));
    purge_wait(stamped, every, now)
}

/// [`purge_expired`], at most once per `every` for everybody who purges this
/// journal: `None` when it stood down, because another process holds the
/// stamp or the stamp says the last purge was less than `every` ago. The
/// stamp is locked for the whole run and read under the lock, and written
/// only when the run completes, so a cancelled purge is still owed.
pub fn purge_expired_if_due(
    trash: &Trash,
    keep_days: u64,
    every: Duration,
    now: SystemTime,
    ctx: &TaskCtx,
) -> Result<Option<Purged>> {
    if !exists(trash.root()) {
        return Ok(None);
    }
    let path = stamp_of(trash);
    let mut stamp = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| DfError::io(&path, e))?
    };
    match stamp.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
        Err(std::fs::TryLockError::Error(e)) => return Err(DfError::io(&path, e)),
    }
    let meta = stamp.metadata().map_err(|e| DfError::io(&path, e))?;
    if purge_wait(read_stamp(&meta), every, now).is_some() {
        return Ok(None);
    }
    let report = purge_expired(trash, keep_days, now, ctx)?;
    stamp
        .set_len(0)
        .and_then(|()| stamp.write_all(b"delightfile purged this trash; the mtime says when\n"))
        .and_then(|()| stamp.set_modified(now))
        .map_err(|e| DfError::io(&path, e))?;
    Ok(Some(report))
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

    // ── `[mgr] trash_keep_days` (M2.35): Linux's aging tests over the journal

    const DAY: Duration = Duration::from_secs(DAY_SECS);

    /// A line with the given date, in a journal that is never touched.
    fn dated(name: &str, deleted_at: &str) -> TrashedItem {
        TrashedItem {
            trash_root: PathBuf::from("/nonexistent/trash-journal"),
            name: OsString::from(name),
            original: PathBuf::from(format!("/Users/someone/{name}")),
            deleted_at: deleted_at.to_string(),
        }
    }

    /// `days` whole days before `now`, as the journal writes it.
    fn days_before(now: SystemTime, days: u64) -> String {
        iso8601_utc(now - Duration::from_secs(days * DAY_SECS))
    }

    fn names(items: &[TrashedItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| i.name.to_string_lossy().into_owned())
            .collect()
    }

    /// The whole table: old enough goes, and nothing else ever does — not a
    /// newer line, not one a second short of its keep, not a date that does
    /// not read, not one from the future, and nothing at all when the keep
    /// is zero. The journal's dates are UTC and its own, so there is no
    /// slack: a line exactly the keep old goes.
    #[test]
    fn only_what_is_older_than_the_keep_expires() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let items = vec![
            dated("ancient", &days_before(now, 400)),
            dated("old", &days_before(now, 31)),
            dated("exactly", &days_before(now, 30)),
            dated(
                "short",
                &iso8601_utc(now - Duration::from_secs(30 * DAY_SECS - 1)),
            ),
            dated("new", &days_before(now, 2)),
            dated("today", &iso8601_utc(now)),
            dated("tomorrow", &iso8601_utc(now + DAY)),
            dated("unreadable", "last tuesday"),
            dated("undated", ""),
        ];
        assert_eq!(
            names(&expired(&items, now, 30)),
            vec!["ancient", "old", "exactly"]
        );
        // A second later, "short" is the keep old too.
        assert_eq!(
            names(&expired(&items, now + Duration::from_secs(1), 30)),
            vec!["ancient", "old", "exactly", "short"]
        );
        // A shorter keep reaches further, and still never the unknowable.
        assert_eq!(
            names(&expired(&items, now, 1)),
            vec!["ancient", "old", "exactly", "short", "new"]
        );
        assert!(expired(&items, now, 0).is_empty());
        assert!(expired(&items, SystemTime::UNIX_EPOCH, 30).is_empty());
        assert!(expired(&items, now, u64::MAX).is_empty());
    }

    /// The date parser reads back exactly what the writer wrote, and turns
    /// everything else into "no date" rather than a panic.
    #[test]
    fn a_deletion_date_round_trips_and_nonsense_is_none() {
        for stamp in [0u64, 1, 951_827_696, 1_756_598_400, 4_102_444_800] {
            let t = SystemTime::UNIX_EPOCH + Duration::from_secs(stamp);
            assert_eq!(parse_deletion_date(&iso8601_utc(t)), Some(t));
        }
        for text in [
            "",
            "yesterday",
            "2026-13-01T00:00:00",
            "2026-08-31T25:00:00",
            "2026/08/31T00:00:00",
        ] {
            assert_eq!(parse_deletion_date(text), None, "{text}");
        }
    }

    /// Put an item where the Trash would have put it — a folder of the
    /// test's own, standing in for `~/.Trash` — and its line in `journal`
    /// with the date given: the way last month's `d` looks on disk.
    fn plant(t: &TempTree, journal: &Path, name: &str, deleted_at: &str, dir: bool) -> PathBuf {
        let location = t.join("Trash").join(name);
        if dir {
            std::fs::create_dir_all(location.join("inner")).unwrap();
            std::fs::write(location.join("inner/deep.txt"), b"deep").unwrap();
        } else {
            std::fs::create_dir_all(t.join("Trash")).unwrap();
            std::fs::write(&location, b"body").unwrap();
        }
        let line = Line {
            deleted_at: deleted_at.to_string(),
            original: t.join("home").join(name),
            location: location.clone(),
        };
        append(journal, &line).unwrap();
        location
    }

    /// Against a journal: the old items go, line and item both, and
    /// everything the table protects is still there afterwards — and so is
    /// what the journal does not record (Finder's own things in the Trash)
    /// and a line this build cannot read, however old its date.
    #[test]
    fn purging_the_old_leaves_everything_else_where_it_was() {
        let t = TempTree::new("mac-trash-keep");
        let journal = t.join("journal");
        let trash = Trash::at(&journal);
        let now = SystemTime::now();
        plant(&t, &journal, "old.txt", &days_before(now, 45), false);
        plant(&t, &journal, "old-folder", &days_before(now, 90), true);
        plant(&t, &journal, "recent.txt", &days_before(now, 3), false);
        plant(&t, &journal, "undated.txt", "", false);
        // Finder's own: in the Trash, in no line.
        t.file("Trash/finders.txt", b"?");
        // A line that is not a line.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .unwrap()
            .write_all(format!("{}\tnot a path\n", days_before(now, 400)).as_bytes())
            .unwrap();

        let report = purge_expired(&trash, 30, now, &ctx()).unwrap();
        assert_eq!(
            report,
            Purged {
                removed: 2,
                failed: 0,
                first_error: None
            }
        );
        for gone in ["old.txt", "old-folder"] {
            assert!(!exists(&t.join("Trash").join(gone)), "{gone} survived");
        }
        for kept in ["recent.txt", "undated.txt", "finders.txt"] {
            assert!(exists(&t.join("Trash").join(kept)), "{kept} was purged");
        }
        assert_eq!(
            names(&trash.list().unwrap()),
            vec!["recent.txt", "undated.txt"]
        );
        let text = std::fs::read_to_string(&journal).unwrap();
        assert!(text.contains("\tnot a path\n"), "{text}");

        // Run again and there is nothing old left to take…
        assert_eq!(
            purge_expired(&trash, 30, now, &ctx()).unwrap(),
            Purged::default()
        );
        // …and a keep of zero never takes anything.
        assert_eq!(
            purge_expired(&trash, 0, now + DAY * 9_000, &ctx()).unwrap(),
            Purged::default()
        );
        assert_eq!(trash.list().unwrap().len(), 2);
    }

    /// A line that changed between the listing and the purge is not the item
    /// the listing chose, and is left alone.
    #[test]
    fn a_record_that_changed_since_the_listing_is_not_purged() {
        let t = TempTree::new("mac-trash-keep-race");
        let journal = t.join("journal");
        let trash = Trash::at(&journal);
        let now = SystemTime::now();
        plant(&t, &journal, "notes.txt", &days_before(now, 45), false);
        let listed = expired(&trash.list().unwrap(), now, 30);
        assert_eq!(listed.len(), 1);
        // Restored and trashed again under the same name, a moment ago.
        rewrite(&journal, |_| false).unwrap();
        plant(&t, &journal, "notes.txt", &iso8601_utc(now), false);
        assert!(!still_recorded(&listed[0]));
        assert!(still_recorded(&trash.list().unwrap()[0]));
    }

    /// Never stamped is owed now; a stamp a day old is owed now; a younger one
    /// waits out the rest of its day; one from the future waits a whole day.
    #[test]
    fn a_stamp_says_how_long_until_the_next_purge() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        let hour = Duration::from_secs(3600);
        assert_eq!(purge_wait(None, DAY, now), None);
        assert_eq!(purge_wait(Some(now - DAY), DAY, now), None);
        assert_eq!(purge_wait(Some(now - DAY * 3), DAY, now), None);
        assert_eq!(purge_wait(Some(now - hour), DAY, now), Some(DAY - hour));
        assert_eq!(purge_wait(Some(now), DAY, now), Some(DAY));
        assert_eq!(purge_wait(Some(now + hour), DAY, now), Some(DAY));
    }

    /// Once a day for everybody: the first run purges and stamps, a second
    /// the same day stands down whatever has aged since, and a day on it runs
    /// again — and the quick question a window asks agrees throughout.
    #[test]
    fn a_purge_runs_once_a_day_whoever_asks() {
        let t = TempTree::new("mac-trash-keep-stamp");
        let journal = t.join("journal");
        let trash = Trash::at(&journal);
        let now = SystemTime::now();
        // Nothing to purge where nothing was ever trashed, and nothing made.
        assert_eq!(
            purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap(),
            None
        );
        assert_eq!(purge_due_in(&trash, DAY, now), Some(DAY));
        assert!(!exists(&journal) && !exists(&stamp_of(&trash)));

        plant(&t, &journal, "old.txt", &days_before(now, 45), false);
        plant(&t, &journal, "recent.txt", &days_before(now, 3), false);
        assert_eq!(purge_due_in(&trash, DAY, now), None, "never purged");
        let first = purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap();
        assert_eq!(first.map(|r| r.removed), Some(1));
        // Stamped with the purge's own `now`, to the filesystem's grain.
        let stamped = std::fs::metadata(stamp_of(&trash))
            .unwrap()
            .modified()
            .unwrap();
        let off = now.duration_since(stamped).unwrap_or_else(|e| e.duration());
        assert!(off < Duration::from_secs(1), "stamped {off:?} away");
        let wait = purge_due_in(&trash, DAY, now).expect("not owed again today");
        assert!(wait > DAY - Duration::from_secs(1));

        // Another process, later the same day: stands down, and the item that
        // has become old since is left for tomorrow's.
        plant(&t, &journal, "older.txt", &days_before(now, 60), false);
        let later = now + Duration::from_secs(3600);
        assert_eq!(
            purge_expired_if_due(&trash, 30, DAY, later, &ctx()).unwrap(),
            None
        );
        assert!(exists(&t.join("Trash/older.txt")));

        let tomorrow = now + DAY + Duration::from_secs(1);
        assert_eq!(purge_due_in(&trash, DAY, tomorrow), None);
        let next = purge_expired_if_due(&trash, 30, DAY, tomorrow, &ctx()).unwrap();
        assert_eq!(next.map(|r| r.removed), Some(1));
        assert!(!exists(&t.join("Trash/older.txt")));
        assert!(exists(&t.join("Trash/recent.txt")));
    }

    /// A purge another process is running is not run twice: the second
    /// stands down silently while the lock is held — and a stamp file that
    /// has only ever been locked, never written, is not a purge.
    #[test]
    fn a_second_purger_stands_down_while_the_first_holds_the_stamp() {
        let t = TempTree::new("mac-trash-keep-lock");
        let journal = t.join("journal");
        let trash = Trash::at(&journal);
        let now = SystemTime::now();
        plant(&t, &journal, "old.txt", &days_before(now, 45), false);
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(stamp_of(&trash))
            .unwrap();
        held.lock().unwrap();
        assert_eq!(
            purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap(),
            None
        );
        assert!(exists(&t.join("Trash/old.txt")), "purged under a lock");
        // Empty, so still owed: the other process never finished.
        assert_eq!(purge_due_in(&trash, DAY, now), None);
        held.unlock().unwrap();
        let report = purge_expired_if_due(&trash, 30, DAY, now, &ctx()).unwrap();
        assert_eq!(report.map(|r| r.removed), Some(1));
    }

    /// A cancelled purge does not stamp: it has not happened, and the next
    /// check runs it.
    #[test]
    fn a_cancelled_purge_is_still_owed() {
        use crate::tasks::{NullSink, TaskFlags};
        let t = TempTree::new("mac-trash-keep-cancel");
        let journal = t.join("journal");
        let trash = Trash::at(&journal);
        let now = SystemTime::now();
        plant(&t, &journal, "old.txt", &days_before(now, 45), false);
        let flags = Arc::new(TaskFlags::new());
        flags.cancel();
        let cancelled = TaskCtx::with_sink(flags, Arc::new(NullSink));
        assert!(matches!(
            purge_expired_if_due(&trash, 30, DAY, now, &cancelled),
            Err(DfError::Cancelled)
        ));
        assert!(exists(&t.join("Trash/old.txt")));
        assert_eq!(purge_due_in(&trash, DAY, now), None);
    }
}
