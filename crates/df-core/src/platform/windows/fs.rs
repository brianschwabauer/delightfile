//! Windows file primitives: the ones `std` can do in a call or two, a file's
//! identity from its handle ([`same_file`], through
//! [`crate::platform::meta::identity`]), whether a folder can be written to
//! by writing to it ([`writable`]), and "no" from the stubs for the three
//! only Linux has (reflink, dropping the cache, `statfs` magic).

use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use crate::{DfError, Result};

pub use crate::platform::stub::fs::*;

/// Make a symbolic link at `link` whose text is `target`. Windows has two
/// kinds, and which one is decided by what the target is — resolved from the
/// link's own folder, as it will be when followed. A target that does not
/// exist gets a file link. Needs Developer Mode or an elevated process;
/// without either the refusal says so, in words a person can act on.
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    let resolved = match link.parent() {
        Some(dir) => dir.join(target),
        None => target.to_path_buf(),
    };
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link).map_err(privilege)
    } else {
        std::os::windows::fs::symlink_file(target, link).map_err(privilege)
    }
}

/// Windows keeps one permission bit: read-only. It is set when `mode` grants
/// nobody write, and cleared otherwise.
pub fn apply_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o222 == 0);
    std::fs::set_permissions(path, permissions)
}

/// Set the access and modification times on `path` itself, never through a
/// symlink: a handle opened for attribute writes on the reparse point, with
/// backup semantics so a directory opens too. A missing time is left alone.
pub fn set_times(path: &Path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| DfError::io(path, e))?;
    let mut times = std::fs::FileTimes::new();
    if let Some(atime) = atime {
        times = times.set_accessed(atime);
    }
    if let Some(mtime) = mtime {
        times = times.set_modified(mtime);
    }
    file.set_times(times).map_err(|e| DfError::io(path, e))
}

/// Nothing to do: a directory on Windows has no `fsync`, and a renamed name is
/// durable once the file system has journalled the rename.
pub fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Write all of `data` at `offset`: `seek_write` until it is all out, since a
/// single call may write less.
pub fn write_all_at(file: &File, mut data: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !data.is_empty() {
        match file.seek_write(data, offset) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(n) => {
                data = &data[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Are these two paths the same file *on disk* — the same volume serial and
/// file index, as `GetFileInformationByHandle` reports them — neither followed
/// if it is a link? The answer that holds however the two are spelled: in
/// another case, through a junction, as a short `8.3` name. An error when
/// either cannot be opened.
pub fn same_file(a: &Path, b: &Path) -> std::io::Result<bool> {
    let of = |path: &Path| {
        let meta = std::fs::symlink_metadata(path)?;
        crate::platform::meta::identity(path, &meta)
    };
    let (a, b) = (of(a)?, of(b)?);
    Ok(a.dev == b.dev && a.ino == b.ino)
}

/// `IO_REPARSE_TAG_MOUNT_POINT`, from `winnt.h`: the tag of a junction, and
/// of a volume mounted in a folder.
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

/// Whether `path` itself is a junction (or a volume mounted in a folder):
/// a link std reports as a directory symlink, whose target is always an
/// absolute path on this machine — not something an archive can carry.
pub fn is_junction(path: &Path) -> bool {
    crate::platform::meta::reparse_tag(path) == Some(IO_REPARSE_TAG_MOUNT_POINT)
}

/// Whether every name still listed in `dir` is on its way out: marked for
/// deletion by a handle another deleter has not yet closed, so the directory
/// will be empty in a moment though removing it now answers "not empty". A
/// name that opens is there to stay, and so is one that cannot be asked
/// about for any other reason; either makes this `false`.
pub fn is_emptying(dir: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const FILE_SHARE_ALL: u32 = 0x7;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let Ok(listing) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in listing {
        let Ok(entry) = entry else {
            return false;
        };
        let opened = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(entry.path());
        match opened {
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || crate::platform::errno::is_delete_pending(&e) => {}
            _ => return false,
        }
    }
    true
}

/// Remove the link at `path` itself, never what it points at. Windows has
/// two kinds: a link to a directory, and a junction, are directories to the
/// file system and go with `RemoveDirectoryW`; a link to a file goes with
/// `DeleteFileW`.
pub fn remove_link(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if std::fs::symlink_metadata(path)?
        .file_type()
        .is_symlink_dir()
    {
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Whether this user may make a name in `dir`, found by making one. Windows
/// has no `access(W_OK)` that answers it: the read-only attribute on a folder
/// means nothing to the file system, and what decides is the folder's ACL, a
/// share's permissions, a read-only volume. So a hidden, temporary file named
/// `.df-write-test-<pid>-<n>` is created there, opened to be deleted when it
/// closes, and closed. A watcher on `dir` sees it come and go.
pub fn writable(dir: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_TEMPORARY: u32 = 0x100;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let probe = dir.join(format!(
        ".df-write-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .access_mode(GENERIC_WRITE | DELETE)
        .attributes(FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_TEMPORARY)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(&probe);
    match made {
        Ok(_file) => true,
        // Somebody else's file of that name: a name could be made here.
        Err(e) => e.kind() == std::io::ErrorKind::AlreadyExists,
    }
}

/// `ERROR_PRIVILEGE_NOT_HELD`: what making a symbolic link answers without
/// Developer Mode or an elevated process.
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

/// The refusal a person can act on, for a link Windows would not let this
/// process make.
fn privilege(e: std::io::Error) -> std::io::Error {
    if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Creating links needs Developer Mode or an elevated process",
        )
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::test_support::TempTree;
    use std::os::windows::fs::FileTypeExt;
    use std::time::Duration;

    /// A link, or `None` when this process may not make one (no Developer
    /// Mode, not elevated), which the refusal's words say.
    fn link(target: &Path, at: &Path) -> Option<()> {
        match symlink(target, at) {
            Ok(()) => Some(()),
            Err(e) => {
                assert_eq!(
                    e.to_string(),
                    "Creating links needs Developer Mode or an elevated process"
                );
                eprintln!("skipping: {e}");
                None
            }
        }
    }

    /// The kind of link follows what the target is, read from the link's own
    /// folder; a target that is not there gets a file link.
    #[test]
    fn a_link_is_of_the_kind_its_target_is() {
        let t = TempTree::new("win-link-kind");
        t.dir("sub/real-dir");
        t.file("sub/real.txt", b"x");
        let kind = |p: &Path| std::fs::symlink_metadata(p).unwrap().file_type();
        if link(Path::new(r"sub\real-dir"), &t.join("to-dir")).is_none() {
            return;
        }
        assert!(kind(&t.join("to-dir")).is_symlink_dir());
        link(Path::new(r"sub\real.txt"), &t.join("to-file")).unwrap();
        assert!(kind(&t.join("to-file")).is_symlink_file());
        link(Path::new("nowhere"), &t.join("dangling")).unwrap();
        assert!(kind(&t.join("dangling")).is_symlink_file());
        // Resolved from the link's folder, not the process's.
        link(Path::new("real-dir"), &t.join(r"sub\beside")).unwrap();
        assert!(kind(&t.join(r"sub\beside")).is_symlink_dir());
    }

    /// A link's own time is set, and its target's is left as it was; a
    /// directory takes a time too.
    #[test]
    fn times_are_set_on_the_link_itself() {
        let t = TempTree::new("win-link-times");
        let target = t.file("target.txt", b"x");
        let before = std::fs::metadata(&target).unwrap().modified().unwrap();
        let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let dir = t.dir("folder");
        set_times(&dir, None, Some(when)).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().modified().unwrap(), when);
        let at = t.join("link.txt");
        if link(&target, &at).is_none() {
            return;
        }
        set_times(&at, Some(when), Some(when)).unwrap();
        let own = std::fs::symlink_metadata(&at).unwrap();
        assert_eq!(own.modified().unwrap(), when);
        assert_eq!(own.accessed().unwrap(), when);
        assert_eq!(
            std::fs::metadata(&target).unwrap().modified().unwrap(),
            before
        );
    }

    /// Write bits granted to nobody set the read-only attribute; any write
    /// bit clears it.
    #[test]
    fn a_mode_without_write_is_the_read_only_attribute() {
        let t = TempTree::new("win-mode");
        let file = t.file("a.txt", b"x");
        apply_mode(&file, 0o444).unwrap();
        assert!(std::fs::metadata(&file).unwrap().permissions().readonly());
        assert!(std::fs::write(&file, b"y").is_err(), "the attribute holds");
        apply_mode(&file, 0o644).unwrap();
        assert!(!std::fs::metadata(&file).unwrap().permissions().readonly());
        std::fs::write(&file, b"y").unwrap();
    }

    /// A folder the process can write to says so and keeps no trace of the
    /// probe; one that is not there cannot be written to.
    #[test]
    fn writability_is_found_by_making_a_name() {
        let t = TempTree::new("win-writable");
        assert!(writable(t.path()));
        assert!(writable(t.path()), "a second probe does not collide");
        let left: Vec<_> = std::fs::read_dir(t.path()).unwrap().collect();
        assert!(left.is_empty(), "the probe deletes itself: {left:?}");
        assert!(!writable(&t.join("missing")));
    }
}
