//! The file primitives Linux and macOS share: making a symlink, setting mode
//! bits and times on a path itself, flushing a directory, writing at an
//! offset, telling two paths apart by inode, and asking the kernel whether a
//! directory can be written to.
//!
//! Each is what `ops`, `sync` and `vfs` did inline before the seam, moved here
//! without a change in what it calls or what it reports.

use std::fs::File;
use std::path::Path;
use std::time::SystemTime;

use super::errno;
use crate::{DfError, Result};

/// Make a symbolic link at `link` whose text is `target`. The target is not
/// looked at: a dangling link is as legal to make as any other.
pub fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Set `path`'s permission bits to `mode` (`chmod(2)`, which follows a
/// symlink; the file-type bits of an `st_mode` are ignored).
pub fn apply_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// `utimensat` on the path itself, never through a symlink.
///
/// `File::set_times` cannot be used: it needs a writable handle, and a
/// directory cannot be opened for writing on Linux. `utimensat` with
/// `AT_SYMLINK_NOFOLLOW` handles files, directories and symlinks with one call.
pub fn set_times(path: &Path, atime: Option<SystemTime>, mtime: Option<SystemTime>) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    fn spec(t: Option<SystemTime>) -> libc::timespec {
        match t.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()) {
            Some(d) => libc::timespec {
                tv_sec: d.as_secs() as libc::time_t,
                tv_nsec: d.subsec_nanos() as i64,
            },
            // Pre-1970 timestamps and unreadable ones both end up here; leaving
            // the value alone beats writing a wrong one. `UTIME_OMIT` is the
            // platform's own number (`0x3ffffffe` on Linux, `-2` on macOS).
            None => libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
        }
    }

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| DfError::Op(format!("{}: path contains a NUL byte", path.display())))?;
    let times = [spec(atime), spec(mtime)];
    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(DfError::io(path, std::io::Error::last_os_error()));
    }
    Ok(())
}

/// `fsync(2)` a directory: open it read-only, flush, close.
///
/// A filesystem that cannot flush a directory at all (`EINVAL`: some FUSE and
/// network mounts, where the server decides) is logged and let through. The
/// name is as safe there as that filesystem ever makes one, and failing every
/// sync onto such a mount would make it a place a sync can never reach.
pub fn sync_dir(dir: &Path) -> Result<()> {
    let handle = File::open(dir).map_err(|e| DfError::io(dir, e))?;
    match handle.sync_all() {
        Ok(()) => Ok(()),
        Err(e) if errno::is_invalid(&e) => {
            log::debug!("{} cannot be flushed as a directory: {e}", dir.display());
            Ok(())
        }
        Err(e) => Err(DfError::io(dir, e)),
    }
}

/// Write all of `data` at `offset`, without moving the file's cursor.
pub fn write_all_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

/// Are these two paths the same file *on disk* (same device and inode),
/// neither followed if it is a link? An error when either cannot be stat'ed.
pub fn same_file(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let (ma, mb) = (std::fs::symlink_metadata(a)?, std::fs::symlink_metadata(b)?);
    Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
}

/// Never: Unix has symlinks and mount points, and no junctions.
pub fn is_junction(_path: &Path) -> bool {
    false
}

/// Remove the symlink at `path` itself, never what it points at: `unlink(2)`,
/// whatever the link points to.
pub fn remove_link(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path)
}

/// `access(2)` for writing: whether this user may make a name in `dir`.
///
/// Asked of the kernel rather than worked out from the mode bits, because
/// ACLs, a read-only mount and root squashing on a network share all answer
/// differently from what the bits say.
pub fn writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // One syscall on a string we own; nothing is written through the pointer.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::access(path.as_ptr(), libc::W_OK) };
    rc == 0
}
