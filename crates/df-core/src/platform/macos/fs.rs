//! macOS's file primitives: the shared Unix ones, and the Linux answers with
//! an ioctl or a magic number that macOS answers its own way.
//!
//! - **A clone copy** is not made yet: [`reflink`] is `false` (M2.2).
//! - **Dropping a file's cached pages** is nothing. `F_NOCACHE` is the nearest
//!   call, and it is not a hint about pages already read: it turns caching off
//!   for the descriptor from then on. A verify reads through the cache, which
//!   still catches everything but a lying medium.
//! - **A network mount** is known by the type name `statfs` gives
//!   (`f_fstypename`: `smbfs`, `nfs`, `macfuse`…) rather than by Linux's
//!   magic numbers; [`magic_of`] answers `f_type` for parity, which is macOS's
//!   own small number and means nothing to `du::REMOTE_FS_MAGIC`.

// One syscall, `statfs`, in one function below whose SAFETY comment says
// what it is trusted with.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub use crate::platform::unix::fs::*;

/// Never, yet (M2.2).
pub fn reflink(_reader: &File, _writer: &File) -> bool {
    false
}

/// Nothing to ask for (see the module note on `F_NOCACHE`).
pub fn forget_cached(_file: &File, _path: &Path) {}

/// `statfs` type names that are not local storage: network filesystems, and
/// FUSE in each of the names macOS has given it (`macfuse`, `osxfuse` before
/// it, and `fuse-t`'s `fuse`). A walk on any of them is a round trip per
/// `stat`, as it is on Linux's.
const REMOTE_TYPES: &[&str] = &["nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp"];
const REMOTE_PREFIXES: &[&str] = &["fuse", "macfuse", "osxfuse"];

/// Whether a filesystem of this `statfs` type name is one an automatic walk
/// should leave alone.
pub fn remote_type(name: &str) -> bool {
    REMOTE_TYPES.contains(&name)
        || REMOTE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Whether `path` sits on a filesystem an automatic walk should leave alone.
///
/// `false` when the question cannot be answered — a path that has just been
/// deleted, a `statfs` that failed — as on Linux.
pub fn is_remote(path: &Path) -> bool {
    statfs(path).is_some_and(|fs| remote_type(&type_name(&fs)))
}

/// The `f_type` of the filesystem `path` is on: macOS's own number, for
/// parity with Linux's magic.
pub fn magic_of(path: &Path) -> Option<i64> {
    statfs(path).map(|fs| i64::from(fs.f_type))
}

/// The filesystem type name `path` is on (`apfs`, `smbfs`, …).
pub fn type_name_of(path: &Path) -> Option<String> {
    statfs(path).map(|fs| type_name(&fs))
}

fn type_name(fs: &libc::statfs) -> String {
    let bytes: Vec<u8> = fs
        .f_fstypename
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn statfs(path: &Path) -> Option<libc::statfs> {
    let raw = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `raw` is a NUL-terminated C string that outlives the call, and
    // `buf` is a correctly sized, correctly aligned `statfs` the kernel fills
    // in. Nothing is read from it unless the call reported success.
    let rc = unsafe { libc::statfs(raw.as_ptr(), buf.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: a zero return means the kernel initialised the struct.
    Some(unsafe { buf.assume_init() })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// The table is the whole feature: a local disk is walked, a share or a
    /// FUSE mount is not.
    #[test]
    fn network_and_fuse_types_are_remote_and_disks_are_not() {
        for local in [
            "apfs", "hfs", "msdos", "exfat", "ntfs", "devfs", "autofs", "",
        ] {
            assert!(!remote_type(local), "{local}");
        }
        for remote in [
            "nfs", "smbfs", "afpfs", "webdav", "cifs", "ftp", "macfuse", "osxfuse", "fuse",
            "fusefs",
        ] {
            assert!(remote_type(remote), "{remote}");
        }
    }

    /// The source tree is on the runner's own disk, which `statfs` names.
    #[test]
    fn a_local_path_is_not_remote() {
        let here = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(!is_remote(here));
        assert!(magic_of(here).is_some());
        let name = type_name_of(here).unwrap();
        assert!(!name.is_empty());
        eprintln!("{} is on {name}", here.display());
    }
}
