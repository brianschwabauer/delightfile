//! What Windows has of a Unix `stat`, and where it keeps the rest.
//!
//! `std`'s stable Windows [`Metadata`] has attributes, three times and a size.
//! The volume serial, file index and link count — what says two names are one
//! file — are behind the unstable `windows_by_handle`, and live in a handle:
//! [`identity`] opens the file (never through a link) and asks
//! `GetFileInformationByHandle`. That is a handle per file, so it is asked only
//! where the answer decides something: [`crate::platform::fs::same_file`], and
//! the du walk's hard-link dedupe for regular files ([`maybe_linked`]).
//!
//! The rest is answered from the `Metadata` alone: no device or inode from it
//! (a boundary check sees one device, which is right, since the walks never
//! follow the reparse points a mounted volume hangs from), one link, a mode made
//! up from the file type and the read-only flag, no owner, the size for the
//! blocks (W4.35 reads the allocation), and the last write for the change time.
//!
//! The `unsafe` calls are `GetFileInformationByHandle` and
//! `GetFileInformationByHandleEx` (a link's reparse tag, for
//! [`crate::platform::fs::is_junction`]), each on a handle this module opened,
//! still owns, and closes by dropping it; the struct each fills is a local of
//! the right type and size, read only when the call says it succeeded.

#![allow(unsafe_code)] // GetFileInformationByHandle(Ex), on handles owned here

use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::time::SystemTime;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};

/// What names one file on one volume, and how many names it has: two paths
/// with the same `dev` and `ino` are the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// The volume serial number.
    pub dev: u64,
    /// The file index on that volume.
    pub ino: u64,
    /// How many names (hard links) the file has.
    pub nlink: u64,
}

const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;

/// No device number in a `Metadata` here: [`identity`] has the volume serial.
pub fn dev(_meta: &Metadata) -> u64 {
    0
}

/// No inode number in a `Metadata` here: [`identity`] has the file index.
pub fn ino(_meta: &Metadata) -> u64 {
    0
}

/// One name, as far as a `Metadata` can say: [`identity`] has the count.
pub fn nlink(_meta: &Metadata) -> u64 {
    1
}

/// The volume serial, file index and link count of `path` itself — a link is
/// not followed, and a directory opens too. `meta` is not needed here: a
/// `Metadata` does not carry them.
pub fn identity(path: &Path, _meta: &Metadata) -> io::Result<Identity> {
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    // SAFETY: an all-zero `BY_HANDLE_FILE_INFORMATION` is a valid value of a
    // plain C struct of integers.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: the handle is `file`'s, open for the length of the call, and
    // `info` is a local the call writes into and nothing else reads meanwhile.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity {
        dev: u64::from(info.dwVolumeSerialNumber),
        ino: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        nlink: u64::from(info.nNumberOfLinks),
    })
}

/// The reparse tag of `path` itself — what kind of link or placeholder it
/// is — or `None` when it is not a reparse point or cannot be opened. For
/// [`crate::platform::fs::is_junction`].
pub(in crate::platform) fn reparse_tag(path: &Path) -> Option<u32> {
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let mut info = FILE_ATTRIBUTE_TAG_INFO {
        FileAttributes: 0,
        ReparseTag: 0,
    };
    // SAFETY: the handle is `file`'s, open for the length of the call; `info`
    // is a local of the struct `FileAttributeTagInfo` names, and the size
    // passed is its size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileAttributeTagInfo,
            std::ptr::addr_of_mut!(info).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    (ok != 0 && info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(info.ReparseTag)
}

/// Whether the file may have names besides this one, as far as `meta` can
/// say: any regular file, since the count is in [`identity`] and not in a
/// `Metadata`.
pub fn maybe_linked(meta: &Metadata) -> bool {
    meta.is_file()
}

/// Hidden by a leading dot, as on Unix, or by the attribute Explorer hides
/// by. `meta` is the row's own, never a link's target's.
pub fn is_hidden(name: &OsStr, meta: &Metadata) -> bool {
    name.to_string_lossy().starts_with('.') || meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

/// An `st_mode` made up from what Windows knows: a link is `lrwxrwxrwx`, a
/// directory `drwxr-xr-x`, a file `-rw-r--r--`, or `-r--r--r--` when it is
/// read-only.
pub fn mode(meta: &Metadata) -> u32 {
    let kind = meta.file_type();
    if kind.is_symlink() {
        S_IFLNK | 0o777
    } else if kind.is_dir() {
        S_IFDIR | 0o755
    } else if meta.permissions().readonly() {
        S_IFREG | 0o444
    } else {
        S_IFREG | 0o644
    }
}

/// No numeric owner on Windows (owners are SIDs).
pub fn uid(_meta: &Metadata) -> u32 {
    0
}

/// No numeric group on Windows.
pub fn gid(_meta: &Metadata) -> u32 {
    0
}

/// The size, for want of the allocation W4.35 reads with
/// `GetCompressedFileSizeW`.
pub fn blocks_bytes(meta: &Metadata) -> u64 {
    meta.len()
}

/// The last write, as seconds and nanoseconds since the epoch: Windows keeps
/// no inode change time.
pub fn change_time(meta: &Metadata) -> (i64, i64) {
    match meta.modified() {
        Ok(modified) => since_epoch(modified),
        Err(_) => (0, 0),
    }
}

/// `when` as a `stat` spells a time: whole seconds, floored, and nanoseconds
/// that are never negative — 0.5 s before the epoch is `(-1, 500_000_000)`,
/// as on Unix. The thumbnail key turns the pair back into a `SystemTime`, and
/// a negative nanosecond field there overflows.
fn since_epoch(when: SystemTime) -> (i64, i64) {
    match when.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, i64::from(d.subsec_nanos())),
        Err(e) => {
            let d = e.duration();
            let (secs, nanos) = (d.as_secs() as i64, i64::from(d.subsec_nanos()));
            if nanos == 0 {
                (-secs, 0)
            } else {
                (-secs - 1, 1_000_000_000 - nanos)
            }
        }
    }
}

/// The last write in whole seconds since the epoch, negative before 1970.
pub fn mtime(meta: &Metadata) -> i64 {
    change_time(meta).0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::test_support::TempTree;

    #[test]
    fn a_hard_link_is_one_file_with_two_names() {
        let t = TempTree::new("win-identity");
        let a = t.file("a.txt", b"x");
        let b = t.join("b.txt");
        std::fs::hard_link(&a, &b).unwrap();
        let other = t.file("other.txt", b"y");
        let of = |p: &Path| identity(p, &std::fs::symlink_metadata(p).unwrap()).unwrap();
        assert_eq!((of(&a).dev, of(&a).ino), (of(&b).dev, of(&b).ino));
        assert_eq!(of(&a).nlink, 2);
        assert_ne!(of(&a).ino, of(&other).ino);
        assert_eq!(of(&other).nlink, 1);
        assert!(of(t.path()).ino != 0, "a directory opens too");
    }

    #[test]
    fn a_time_before_the_epoch_is_floored_as_a_stat_floors_it() {
        use std::time::Duration;
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(since_epoch(epoch + Duration::new(5, 7)), (5, 7));
        assert_eq!(since_epoch(epoch - Duration::from_secs(2)), (-2, 0));
        assert_eq!(
            since_epoch(epoch - Duration::from_millis(500)),
            (-1, 500_000_000)
        );
    }

    #[test]
    fn the_hidden_attribute_hides_as_a_dot_does() {
        let t = TempTree::new("win-hidden");
        let plain = t.file("plain.txt", b"x");
        let meta = std::fs::symlink_metadata(&plain).unwrap();
        assert!(!is_hidden(OsStr::new("plain.txt"), &meta));
        assert!(is_hidden(OsStr::new(".dotted"), &meta));
        let status = std::process::Command::new("attrib")
            .arg("+h")
            .arg(&plain)
            .status();
        if status.is_ok_and(|s| s.success()) {
            let meta = std::fs::symlink_metadata(&plain).unwrap();
            assert!(is_hidden(OsStr::new("plain.txt"), &meta));
        }
    }
}
