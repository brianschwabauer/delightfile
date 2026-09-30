//! The Recycle Bin: where `d` sends a file on Windows.
//!
//! One shell call does it: `SHFileOperationW` with `FO_DELETE` and
//! `FOF_ALLOWUNDO`, which is what Explorer's own Delete key has done since
//! Windows 95, and the one way to recycle that needs no COM object
//! (`04-windows.md` rules hand-written COM out of this phase). The file goes
//! wherever the shell puts it — the drive's `$Recycle.Bin`, under the user's
//! SID, renamed to a `$R…` name the call does not report — so what comes back
//! is a [`TrashedItem`] that remembers where the file *was* and when it went,
//! with no [`TrashedItem::location`]: nothing here can find it in the bin
//! again. That is the price of the one call, and it is paid in two places:
//!
//! - **No restore and no listing.** Reading the bin is `IShellFolder` over
//!   `CSIDL_BITBUCKET`, and putting an item back is its context menu's
//!   "Restore" verb — COM both, deferred as W4.28. So [`Trash::list`] is empty,
//!   and [`restore`] and [`purge`] refuse in words; `u` after a `d` says where
//!   to go instead (W4.8), and the trash view is Explorer's.
//! - **No aging.** The bin keeps its own size limit, and Storage Sense ages it;
//!   [`purge_due_in`] is never due.
//! - **Emptying is the whole bin.** "Empty trash" says how many items the
//!   bins of every drive hold and how much they weigh ([`bin_size`],
//!   `SHQueryRecycleBinW`), since there is no view to list them in, and on a
//!   yes empties them all ([`empty_bin`], `SHEmptyRecycleBinW`).
//!
//! **Where there is no bin, nothing is sent.** The same call on a drive without
//! a Recycle Bin — a USB stick, a memory card, a network share — deletes the
//! file for good with no more warning than the shell's own question (below),
//! where `d` owes the app's permanent-delete card. So a path is recycled only on a fixed
//! drive (`GetDriveTypeW` of the volume it is on), and anywhere else
//! [`for_path`] and [`Trash::trash`] refuse, and the app falls back to the
//! permanent-delete confirm it shows for remote rows.
//!
//! **Where the bin has no room, the shell asks.** A file larger than its
//! drive's bin may hold is not recycled but deleted for good, and nothing
//! here can tell before the call. `FOF_WANTNUKEWARNING`, which overrides
//! `FOF_NOCONFIRMATION` for that one question, has the shell ask in its own
//! dialog ("permanently delete?"); a No leaves the file where it was and
//! the call reports itself aborted, which [`Trash::trash`] refuses as
//! cancelled.
//!
//! `SHFileOperationW` has no long-path support and rejects a `\\?\` path
//! outright, so the path it is handed is the plain spelling of the same file,
//! under `MAX_PATH`, or the call is not made.
//!
//! The `unsafe` is `SHFileOperationW`, `SHQueryRecycleBinW`,
//! `SHEmptyRecycleBinW`, `GetVolumePathNameW` and `GetDriveTypeW`, written to
//! the same three rules as the other islands:
//!
//! 1. Nothing here owns a handle: the calls take strings and a struct, and
//!    return.
//! 2. Every string and struct a call reads or writes is a local that outlives
//!    the call — the wide strings are built NUL-terminated (`pFrom`
//!    double-NUL-terminated) before it and dropped after it.
//! 3. Every return is checked; nothing is assumed to succeed, and no `unsafe`
//!    escapes the file.

#![allow(unsafe_code)] // SHFileOperationW, GetVolumePathNameW, GetDriveTypeW on locals

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::time::{Duration, SystemTime};

use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumePathNameW};
use windows_sys::Win32::UI::Shell::{
    SHEmptyRecycleBinW, SHFileOperationW, SHQueryRecycleBinW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION,
    FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE, SHERB_NOCONFIRMATION,
    SHFILEOPSTRUCTW, SHQUERYRBINFO,
};

use crate::ops::{exists, normalize};
use crate::tasks::TaskCtx;
use crate::{DfError, Result};

/// `DRIVE_FIXED`, from `winbase.h`: a hard disk, internal or external — the one
/// kind of drive Windows keeps a Recycle Bin on.
const DRIVE_FIXED: u32 = 3;

/// The longest path `SHFileOperationW` takes, in UTF-16 units with its NUL.
const MAX_PATH: usize = 260;

/// The name of a drive's Recycle Bin folder, at its root.
const BIN_FOLDER: &str = "$Recycle.Bin";

/// What a recycle asks of the shell: into the bin (`FOF_ALLOWUNDO`), with no
/// progress window, no questions and no error dialog of its own, but for its
/// warning before a file too big for the bin is deleted for good
/// (`FOF_WANTNUKEWARNING`, which overrides `FOF_NOCONFIRMATION` for that one
/// question). `SHFILEOPSTRUCTW` holds the flags in 16 bits.
const RECYCLE_FLAGS: u16 =
    (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING) as u16;

/// One recycled thing: where it came from and when it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// The `$Recycle.Bin` of the drive it went to.
    pub trash_root: PathBuf,
    /// Its name where it was: the bin's own name for it is not reported.
    pub name: OsString,
    /// Where it came from, absolute.
    pub original: PathBuf,
    /// `YYYY-MM-DDThh:mm:ss`, UTC.
    pub deleted_at: String,
}

impl TrashedItem {
    /// Where the item is now: not known here (see the module note), so an
    /// empty path, which exists nowhere.
    pub fn location(&self) -> PathBuf {
        PathBuf::new()
    }

    /// [`TrashedItem::location`].
    pub fn files_path(&self) -> PathBuf {
        self.location()
    }

    /// [`TrashedItem::location`]: the bin's record is not this program's.
    pub fn info_path(&self) -> PathBuf {
        self.location()
    }

    /// Never: an orphan is a freedesktop trash's file without its record.
    pub fn is_orphan(&self) -> bool {
        false
    }
}

/// The Recycle Bin of one drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    /// The bin at `root`: a value, so the undo journal can name the drive an
    /// item went to. The shell picks the bin from the path, so any `Trash`
    /// recycles to the right one.
    pub fn at(root: impl Into<PathBuf>) -> Trash {
        Trash { root: root.into() }
    }

    /// The system drive's bin (`%SystemDrive%\$Recycle.Bin`).
    pub fn home() -> Result<Trash> {
        let drive = std::env::var_os("SystemDrive")
            .filter(|drive| !drive.is_empty())
            .unwrap_or_else(|| OsString::from("C:"));
        let mut root = PathBuf::from(drive);
        root.push("\\");
        Ok(Trash::at(root.join(BIN_FOLDER)))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root itself: the bin's folders are the shell's.
    pub fn files_dir(&self) -> PathBuf {
        self.root.clone()
    }

    /// Recycle `path`: `SHFileOperationW(FO_DELETE)` with `FOF_ALLOWUNDO`,
    /// silent, no confirmation and no error dialog — except the shell's own
    /// question before a file too big for the bin is deleted for good
    /// (`FOF_WANTNUKEWARNING`), whose No is refused as cancelled. Refused,
    /// before the call, for a path that is not there, one the permanent
    /// delete's rails refuse, one on a drive with no bin, and one too long or
    /// of a form the shell cannot take. The item has no
    /// [`TrashedItem::location`].
    pub fn trash(&self, path: &Path, _ctx: &TaskCtx) -> Result<TrashedItem> {
        if !exists(path) {
            return Err(DfError::io(
                path,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            ));
        }
        // The same rails as a permanent delete, as on Linux.
        crate::ops::delete::check_deletable_here(path)?;
        let original = normalize(path);
        if !recycles(&original) {
            return Err(no_bin(&original));
        }
        let from = shell_from(&original)?;
        let mut operation = SHFILEOPSTRUCTW {
            hwnd: 0,
            wFunc: FO_DELETE,
            pFrom: from.as_ptr(),
            pTo: std::ptr::null(),
            fFlags: RECYCLE_FLAGS,
            fAnyOperationsAborted: 0,
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: std::ptr::null(),
        };
        // SAFETY: `operation` is a local whose `pFrom` points into `from`, a
        // double-NUL-terminated wide string that lives past the call; `pTo`,
        // the name mappings and the title are null, as FO_DELETE allows. The
        // call writes only `fAnyOperationsAborted` and `hNameMappings` (never
        // asked for, so never allocated).
        let code = unsafe { SHFileOperationW(&mut operation) };
        drop(from);
        if code != 0 {
            return Err(DfError::Op(format!(
                "{}: the Recycle Bin refused it (error {code:#x})",
                crate::path::display(&original)
            )));
        }
        if operation.fAnyOperationsAborted != 0 {
            return Err(DfError::Op(format!(
                "{}: moving it to the Recycle Bin was cancelled",
                crate::path::display(&original)
            )));
        }
        if exists(&original) {
            return Err(DfError::Op(format!(
                "{}: still there after the Recycle Bin took it",
                crate::path::display(&original)
            )));
        }
        let name = original.file_name().unwrap_or_default().to_os_string();
        Ok(TrashedItem {
            trash_root: self.root.clone(),
            name,
            original,
            deleted_at: iso8601_utc(SystemTime::now()),
        })
    }

    /// Nothing this program can see: reading the bin is W4.28's.
    pub fn list(&self) -> Result<Vec<TrashedItem>> {
        Ok(Vec::new())
    }

    /// Refused, as [`restore`].
    pub fn restore(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<PathBuf> {
        restore(item, ctx)
    }

    /// Refused, as [`purge`].
    pub fn purge(&self, item: &TrashedItem, ctx: &TaskCtx) -> Result<()> {
        purge(item, ctx)
    }
}

/// Refused: an item is put back from the Recycle Bin in Explorer (W4.28).
pub fn restore(_item: &TrashedItem, _ctx: &TaskCtx) -> Result<PathBuf> {
    Err(DfError::Unsupported("Restoring from the Recycle Bin"))
}

/// Refused, for the reason [`restore`] is: the item cannot be found in the bin.
pub fn purge(_item: &TrashedItem, _ctx: &TaskCtx) -> Result<()> {
    Err(DfError::Unsupported("Restoring from the Recycle Bin"))
}

/// The bin `path` would go to: its drive's, when that drive keeps one;
/// refused otherwise, with the drive named.
pub fn for_path(path: &Path) -> Result<Trash> {
    let original = normalize(path);
    if !recycles(&original) {
        return Err(no_bin(&original));
    }
    Ok(Trash::at(volume_of(&original).join(BIN_FOLDER)))
}

/// Whether what is under `dest` can be recycled: `dest` is on a fixed drive.
/// Removable media and network shares have no bin; a mirror's extras there
/// are deleted for good, after the card has said so.
pub fn available_for(dest: &Path) -> bool {
    recycles(&normalize(dest))
}

/// The bin a mirror's extras under `dest` go to, as [`for_path`].
pub fn for_sync(dest: &Path) -> Result<Trash> {
    for_path(dest)
}

/// What a purge of old items came to: how many went, and how many would not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Purged {
    pub removed: usize,
    pub failed: usize,
    /// The first refusal, word for word, for the task panel and the toast.
    pub first_error: Option<String>,
}

/// Refused: the bin ages itself (its size limit, and Storage Sense).
pub fn purge_expired(
    _trash: &Trash,
    _keep_days: u64,
    _now: SystemTime,
    _ctx: &TaskCtx,
) -> Result<Purged> {
    Err(DfError::Unsupported(
        "Emptying old items from the Recycle Bin",
    ))
}

/// Refused, as [`purge_expired`].
pub fn purge_expired_if_due(
    _trash: &Trash,
    _keep_days: u64,
    _every: Duration,
    _now: SystemTime,
    _ctx: &TaskCtx,
) -> Result<Option<Purged>> {
    Err(DfError::Unsupported(
        "Emptying old items from the Recycle Bin",
    ))
}

/// Never owed a purge: asked again in `every`.
pub fn purge_due_in(_trash: &Trash, every: Duration, _now: SystemTime) -> Option<Duration> {
    Some(every)
}

/// What the Recycle Bin holds, on every drive, as the shell counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinSize {
    pub items: u64,
    pub bytes: u64,
}

/// How many items the Recycle Bins of every drive hold, and their size —
/// `SHQueryRecycleBinW` over all of them. What "Empty trash" says before it
/// asks, there being no view here to list the items from (W4.8).
pub fn bin_size() -> Result<BinSize> {
    let mut info = SHQUERYRBINFO {
        cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
        i64Size: 0,
        i64NumItems: 0,
    };
    // SAFETY: a null root asks for every drive's bin; `info` is a local
    // with its size set, as the call requires, and outlives it.
    let code = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    if code != 0 {
        return Err(DfError::Op(format!(
            "the Recycle Bin could not be counted (error {code:#x})"
        )));
    }
    Ok(BinSize {
        items: u64::try_from(info.i64NumItems).unwrap_or(0),
        bytes: u64::try_from(info.i64Size).unwrap_or(0),
    })
}

/// Empty the Recycle Bin of every drive, for good: `SHEmptyRecycleBinW` with
/// `SHERB_NOCONFIRMATION`, the app's own confirm having asked. A bin that is
/// already empty is not asked, and is `Ok`.
pub fn empty_bin() -> Result<()> {
    if bin_size()?.items == 0 {
        return Ok(());
    }
    // SAFETY: no window and a null root (every drive's bin); the call reads
    // nothing of ours.
    let code = unsafe { SHEmptyRecycleBinW(0, std::ptr::null(), SHERB_NOCONFIRMATION) };
    if code != 0 {
        return Err(DfError::Op(format!(
            "the Recycle Bin could not be emptied (error {code:#x})"
        )));
    }
    Ok(())
}

/// The refusal for a path whose drive keeps no bin.
fn no_bin(path: &Path) -> DfError {
    DfError::Op(format!(
        "{} has no Recycle Bin",
        crate::path::display(&volume_of(path))
    ))
}

/// Whether `path` is on a drive with a Recycle Bin.
fn recycles(path: &Path) -> bool {
    has_bin(drive_type(path))
}

/// Whether a drive of this `GetDriveTypeW` kind keeps a Recycle Bin: only a
/// fixed one. A removable drive (a USB stick, a card) and a network share do
/// not, and the shell deletes from them for good.
fn has_bin(kind: u32) -> bool {
    kind == DRIVE_FIXED
}

/// `GetDriveTypeW` of the volume `path` is on.
fn drive_type(path: &Path) -> u32 {
    let root = wide(volume_of(path).as_os_str());
    // SAFETY: `root` is a NUL-terminated wide string that outlives the call,
    // which only reads it.
    unsafe { GetDriveTypeW(root.as_ptr()) }
}

/// The root of the volume `path` is on, with its trailing separator — `C:\`,
/// `\\server\share\`, or a folder a volume is mounted in — by
/// `GetVolumePathNameW`, or by the path's own root when that fails.
fn volume_of(path: &Path) -> PathBuf {
    let name = wide(path.as_os_str());
    let mut out = vec![0u16; 1024];
    // SAFETY: `name` is NUL-terminated and `out` is a writable buffer of the
    // length passed; both outlive the call. The result is read only on
    // success, up to its NUL.
    let ok = unsafe { GetVolumePathNameW(name.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    if ok != 0 {
        if let Some(end) = out.iter().position(|&unit| unit == 0) {
            use std::os::windows::ffi::OsStringExt;
            return PathBuf::from(OsString::from_wide(&out[..end]));
        }
    }
    crate::path::root_of(path)
}

/// `s` as a NUL-terminated wide string.
fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// `path` as `SHFileOperationW` reads a `pFrom`: its plain spelling, under
/// [`MAX_PATH`], double-NUL-terminated.
fn shell_from(path: &Path) -> Result<Vec<u16>> {
    let Some(plain) = plain_form(path) else {
        return Err(DfError::Op(format!(
            "{}: the Recycle Bin cannot take a path written this way",
            crate::path::display(path)
        )));
    };
    let mut units: Vec<u16> = plain.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(DfError::Op(format!(
            "{}: a path cannot contain a NUL character",
            crate::path::display(path)
        )));
    }
    if units.len() >= MAX_PATH {
        return Err(DfError::Op("path too long for the Recycle Bin".to_string()));
    }
    units.extend([0, 0]);
    Ok(units)
}

/// The plain spelling of `path`, which names the same file: `\\?\C:\x` is
/// `C:\x` and `\\?\UNC\s\sh\x` is `\\s\sh\x`. `None` for a verbatim path the
/// plain form would read differently — a `.` or `..` name, a `/` inside a
/// name, a name ending in a dot or a space, all of which a plain path
/// normalizes away — and for the verbatim forms with no plain one (a volume
/// GUID, a device).
fn plain_form(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Some(path.to_path_buf());
    };
    let mut plain = match prefix.kind() {
        Prefix::Disk(_) | Prefix::UNC(..) => return Some(path.to_path_buf()),
        Prefix::VerbatimDisk(letter) => PathBuf::from(format!("{}:\\", char::from(letter))),
        Prefix::VerbatimUNC(server, share) => {
            let mut root = OsString::from(r"\\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
        Prefix::Verbatim(_) | Prefix::DeviceNS(_) => return None,
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let text = name.to_str()?;
                if text.contains('/') || text.ends_with('.') || text.ends_with(' ') {
                    return None;
                }
                plain.push(name);
            }
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    Some(plain)
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
    use crate::test_support::TempTree;

    /// The tests that put things in the bin, or empty it, one at a time: a
    /// count taken around a trash must not see an emptying.
    static BIN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    /// How many items the current user's bin on `path`'s drive holds, as the
    /// shell counts them.
    fn bin_count(path: &Path) -> Option<i64> {
        let root = wide(volume_of(path).as_os_str());
        let mut info = SHQUERYRBINFO {
            cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
            i64Size: 0,
            i64NumItems: 0,
        };
        // SAFETY: `root` is NUL-terminated and `info` a local with its size
        // set, as the call requires; both outlive it.
        let hr = unsafe { SHQueryRecycleBinW(root.as_ptr(), &mut info) };
        (hr == 0).then_some(info.i64NumItems)
    }

    /// A file on the temp drive leaves its folder and lands in the bin, which
    /// the shell's own count says; the item remembers where it was and has
    /// nowhere it is now.
    #[test]
    fn a_trashed_file_goes_to_the_recycle_bin() {
        let _bin = BIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let t = TempTree::new("win-recycle");
        let file = t.file("doomed.txt", b"bye");
        assert!(
            available_for(t.path()),
            "the temp folder is on a fixed drive"
        );
        let before = bin_count(&file);
        let bin = for_path(&file).unwrap();
        assert!(bin.root().ends_with(BIN_FOLDER), "{}", bin.root().display());
        let item = bin.trash(&file, &ctx()).unwrap();
        assert!(!exists(&file), "gone from its folder");
        assert_eq!(item.original, normalize(&file));
        assert_eq!(item.name, "doomed.txt");
        assert!(item.location().as_os_str().is_empty());
        assert!(parse_deletion_date(&item.deleted_at).is_some());
        let after = bin_count(&file);
        eprintln!("items in the bin before and after: {before:?}, {after:?}");
        if let (Some(before), Some(after)) = (before, after) {
            assert!(after > before, "into the bin, not deleted for good");
        }
    }

    /// A folder goes whole, and a path that is not there is refused before
    /// the shell is asked.
    #[test]
    fn a_folder_goes_whole_and_a_missing_path_is_refused() {
        let _bin = BIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let t = TempTree::new("win-recycle-dir");
        let dir = t.dir("folder");
        std::fs::write(dir.join("inner.txt"), b"x").unwrap();
        Trash::home().unwrap().trash(&dir, &ctx()).unwrap();
        assert!(!exists(&dir));
        let missing = Trash::home()
            .unwrap()
            .trash(&t.join("never-was"), &ctx())
            .unwrap_err();
        assert!(matches!(missing, DfError::Io { .. }), "{missing}");
    }

    /// "Empty trash" counts the bin and empties it: something trashed is
    /// counted, and gone after, and an empty bin empties without a word.
    #[test]
    fn the_bin_is_counted_and_emptied() {
        let _bin = BIN
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let t = TempTree::new("win-recycle-empty");
        let file = t.file("counted.txt", &[b'x'; 4096]);
        for_path(&file).unwrap().trash(&file, &ctx()).unwrap();
        let full = bin_size().unwrap();
        assert!(full.items >= 1, "{full:?}");
        assert!(full.bytes >= 4096, "{full:?}");
        empty_bin().unwrap();
        assert_eq!(bin_size().unwrap().items, 0);
        empty_bin().unwrap();
    }

    /// What cannot be done here says so, and the bin lists nothing.
    #[test]
    fn restoring_and_listing_are_refused_in_words() {
        let item = TrashedItem {
            trash_root: PathBuf::from(r"C:\$Recycle.Bin"),
            name: OsString::from("a.txt"),
            original: PathBuf::from(r"C:\a.txt"),
            deleted_at: iso8601_utc(SystemTime::now()),
        };
        let refusal = "Restoring from the Recycle Bin is not available on this platform";
        assert_eq!(restore(&item, &ctx()).unwrap_err().to_string(), refusal);
        assert_eq!(purge(&item, &ctx()).unwrap_err().to_string(), refusal);
        let bin = Trash::home().unwrap();
        assert!(bin.list().unwrap().is_empty());
        assert!(!item.is_orphan());
        assert_eq!(
            purge_due_in(&bin, Duration::from_secs(60), SystemTime::now()),
            Some(Duration::from_secs(60))
        );
    }

    /// The shell is asked to recycle, quietly, and to warn before it deletes
    /// a file too big for the bin for good; every flag survives the 16 bits
    /// the struct holds them in. The warning itself is a dialog, which no
    /// runner shows: 07-verification.md §5.4 is where it is seen.
    #[test]
    fn a_recycle_asks_before_it_deletes_for_good() {
        let wanted =
            FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI | FOF_WANTNUKEWARNING;
        assert_eq!(u32::from(RECYCLE_FLAGS), wanted);
    }

    /// Only a fixed drive keeps a bin: `DRIVE_REMOVABLE` (2), `DRIVE_REMOTE`
    /// (4), `DRIVE_CDROM` (5) and the unknowns do not.
    #[test]
    fn only_a_fixed_drive_recycles() {
        assert!(has_bin(DRIVE_FIXED));
        for kind in [0, 1, 2, 4, 5, 6] {
            assert!(!has_bin(kind), "{kind}");
        }
    }

    /// The shell is handed the plain spelling of the same file, never a
    /// verbatim one, and never a path it would read as another.
    #[test]
    fn the_shell_gets_the_plain_spelling_or_nothing() {
        let p = |s: &str| Some(PathBuf::from(s));
        assert_eq!(plain_form(Path::new(r"C:\a\b.txt")), p(r"C:\a\b.txt"));
        assert_eq!(plain_form(Path::new(r"\\?\C:\a\b.txt")), p(r"C:\a\b.txt"));
        assert_eq!(plain_form(Path::new(r"\\?\UNC\s\sh\x")), p(r"\\s\sh\x"));
        assert_eq!(plain_form(Path::new(r"\\?\C:\a.")), None);
        assert_eq!(plain_form(Path::new(r"\\?\C:\a\..\b")), None);
        assert_eq!(plain_form(Path::new(r"\\?\Volume{0}\a")), None);
        assert_eq!(plain_form(Path::new(r"\\.\COM1")), None);

        let from = shell_from(Path::new(r"C:\a")).unwrap();
        assert_eq!(from, [67, 58, 92, 97, 0, 0], "double-NUL-terminated");
        let long = PathBuf::from(format!(r"C:\{}", "x".repeat(MAX_PATH)));
        assert_eq!(
            shell_from(&long).unwrap_err().to_string(),
            "path too long for the Recycle Bin"
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
