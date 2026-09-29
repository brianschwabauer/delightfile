//! Linux's file primitives: the shared Unix ones, plus the three that only
//! Linux has — a reflink by `FICLONE`, dropping a file's cached pages with
//! `posix_fadvise`, and a filesystem's `f_type` magic from `statfs`.

use std::fs::File;
use std::path::Path;

pub use crate::platform::unix::fs::*;

/// `FICLONE`: `_IOW(0x94, 9, int)`, the reflink ioctl.
///
/// Spelled out rather than pulled from a crate because it is one number and its
/// derivation is right here: direction `_IOC_WRITE` (1) << 30, size 4 << 16,
/// type `0x94` << 8, number 9 — `0x4000_0000 | 0x0004_0000 | 0x9400 | 0x09`.
const FICLONE: libc::c_ulong = 0x4004_9409;

/// Ask the kernel to share the source's extents with the destination.
///
/// Returns `false` for every failure, and that is the whole error handling:
/// `EOPNOTSUPP` (ext4, tmpfs), `EXDEV` (different filesystem), `EINVAL` (not a
/// regular file, or a destination that is not empty) and anything else all mean
/// exactly one thing to the caller — copy it the long way. The destination file
/// is untouched on failure, so falling through costs nothing.
pub fn reflink(reader: &File, writer: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    // The only unsafe in df-core besides `getuid`: one ioctl on two fds we own,
    // with no pointers involved and no way to alias anything.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::ioctl(writer.as_raw_fd(), FICLONE, reader.as_raw_fd()) };
    if rc != 0 {
        log::trace!(
            "FICLONE unavailable ({}), falling back to a chunked copy",
            std::io::Error::last_os_error()
        );
    }
    rc == 0
}

/// Ask the kernel to drop a file's cached pages. Advisory: a filesystem that
/// ignores it (some FUSE mounts, where the server holds the cache) is logged
/// and read anyway, since a read through the cache still catches everything
/// but a lying medium.
pub fn forget_cached(file: &File, path: &Path) {
    use std::os::unix::io::AsRawFd;
    // One syscall on an fd we own, with no pointers involved.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if rc != 0 {
        log::debug!(
            "{}: the page cache could not be dropped ({})",
            path.display(),
            std::io::Error::from_raw_os_error(rc)
        );
    }
}

/// Whether `path` sits on a filesystem an automatic walk should leave alone.
///
/// `false` when the question cannot be answered — a path that has just been
/// deleted, a `statfs` that failed. Refusing to measure on a failed syscall
/// would turn one transient error into a column of em dashes.
pub fn is_remote(path: &Path) -> bool {
    magic_of(path).is_some_and(|magic| crate::du::REMOTE_FS_MAGIC.contains(&magic))
}

/// The `f_type` of the filesystem `path` is on.
///
/// Split out so the table above can be checked against a real mount by hand
/// (`stat -f -c %t`) without the answer being hidden behind a boolean.
#[allow(unsafe_code)]
pub fn magic_of(path: &Path) -> Option<i64> {
    use std::os::unix::ffi::OsStrExt;
    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `raw` is a NUL-terminated C string that outlives the call, and
    // `buf` is a correctly sized, correctly aligned `statfs` the kernel fills
    // in. Nothing is read from it unless the call reported success.
    let rc = unsafe { libc::statfs(raw.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: a zero return means the kernel initialised the struct.
    let buf = unsafe { buf.assume_init() };
    // `f_type` is `__fsword_t`, which is `i64` on 64-bit Linux and `i32`
    // elsewhere — so the cast is a no-op on the machine clippy is reading and
    // the only thing making this compile on the others.
    #[allow(clippy::unnecessary_cast)]
    Some(buf.f_type as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one filesystem a test can rely on being there, and the one answer
    /// that matters: a walk of the source tree is allowed to start.
    #[test]
    fn a_local_path_is_not_remote() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(!is_remote(here));
        assert!(magic_of(here).is_some());
    }
}
