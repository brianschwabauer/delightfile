//! macOS's file primitives: the shared Unix ones, and the three Linux answers
//! with an ioctl or a magic number that macOS answers its own way.
//!
//! - **A clone copy** is `fclonefileat` on APFS, which makes a new file
//!   sharing the source's blocks — a 40 GB video duplicated in a millisecond,
//!   as `FICLONE` does on btrfs. It makes the file rather than filling one, so
//!   the copy asks for it *before* opening its destination
//!   ([`clone_before_open`]), and [`reflink`], which fills an open file, is
//!   never it.
//! - **Dropping a file's cached pages** is nothing. `F_NOCACHE` is the nearest
//!   call, and it is not a hint about pages already read: it turns caching off
//!   for the descriptor from then on. A verify reads through the cache, which
//!   still catches everything but a lying medium.
//! - **A network mount** is known by the type name `statfs` gives
//!   (`f_fstypename`: `smbfs`, `nfs`, `macfuse`…) rather than by Linux's
//!   magic numbers; [`magic_of`] answers `f_type` for parity, which is macOS's
//!   own small number and means nothing to `du::REMOTE_FS_MAGIC`.

// Two syscalls, `fclonefileat` and `statfs`, each in one function below whose
// SAFETY comment says what it is trusted with.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;

pub use crate::platform::unix::fs::*;

/// `CLONE_NOFOLLOW`, from `<sys/clonefile.h>`: do not follow the source if it
/// is a link. libc 0.2.189 has `fclonefileat` but not its flags. The source
/// here is an open descriptor, so there is nothing left to follow; the flag
/// says so to the kernel as well.
const CLONE_NOFOLLOW: u32 = 0x0001;

/// Never: macOS clones a file into being, not into an open one
/// ([`clone_before_open`]).
pub fn reflink(_reader: &File, _writer: &File) -> bool {
    false
}

/// Make `dst` a clone of the file `reader` is open on: a new file sharing its
/// blocks, carrying its mode, times and attributes. `dst` must not exist.
///
/// `Ok(true)` when the clone was made, and the copy is done. `Ok(false)` when
/// the filesystem cannot clone this — another volume (`EXDEV`), a filesystem
/// that is not APFS (`ENOTSUP`), anything the kernel calls invalid (`EINVAL`)
/// — and nothing was made, so the caller copies the long way. Anything else
/// is a real error, `EEXIST` above all: the caller has settled what happens to
/// a name in the way before it gets here.
pub fn clone_before_open(reader: &File, dst: &Path) -> io::Result<bool> {
    let c_dst = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: the source is a descriptor `reader` owns and keeps open for the
    // call, and `c_dst` is a NUL-terminated string that outlives it.
    let rc = unsafe {
        libc::fclonefileat(
            reader.as_raw_fd(),
            libc::AT_FDCWD,
            c_dst.as_ptr(),
            CLONE_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL) => {
            log::trace!("clonefile unavailable ({e}), falling back to a chunked copy");
            Ok(false)
        }
        _ => Err(e),
    }
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
    use crate::ops::fixture::TempTree;
    use crate::tasks::TaskCtx;
    use std::process::Command;
    use std::time::{Duration, Instant};

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

    /// A 100 MiB file cloned on APFS is a copy in well under the time it takes
    /// to read it — the clone is metadata, whatever the size.
    #[test]
    fn a_copy_on_apfs_is_a_clone() {
        let t = TempTree::new("clone");
        if type_name_of(t.path()).as_deref() != Some("apfs") {
            eprintln!(
                "skipping: {} is not on APFS ({:?})",
                t.path().display(),
                type_name_of(t.path())
            );
            return;
        }
        let body: Vec<u8> = (0..100 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let src = t.file("big.bin", &body);
        // On the disk before the clock starts: a clone of a file with dirty
        // pages waits for them, and that wait is the write's, not the copy's.
        File::open(&src).unwrap().sync_all().unwrap();
        let dst = t.join("big-copy.bin");
        let started = Instant::now();
        crate::ops::copy::copy_tree(&src, &dst, &TaskCtx::detached(), false).unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_millis(50), "took {took:?}");
        assert_eq!(std::fs::read(&dst).unwrap(), body);

        // Straight through the platform: a clone is `true`, a name in the
        // way is an error rather than a quiet fallback.
        let reader = File::open(&src).unwrap();
        let again = t.join("again.bin");
        assert!(clone_before_open(&reader, &again).unwrap());
        let taken = clone_before_open(&reader, &again).unwrap_err();
        assert_eq!(taken.raw_os_error(), Some(libc::EEXIST));
    }

    /// A clone onto another volume cannot be one: `false`, nothing made, and
    /// the copy takes the long way with the same bytes. The other volume is a
    /// small HFS+ disk image, which cannot clone at all either.
    #[test]
    fn a_copy_across_volumes_falls_back_to_the_chunked_copy() {
        let t = TempTree::new("clone-xvol");
        let image = t.join("other.dmg");
        let mount = t.dir("mnt");
        let made = Command::new("hdiutil")
            .args(["create", "-quiet", "-size", "8m", "-fs", "HFS+", "-volname"])
            .arg("dfclone")
            .arg(&image)
            .status()
            .is_ok_and(|s| s.success())
            && Command::new("hdiutil")
                .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
                .arg(&mount)
                .arg(&image)
                .status()
                .is_ok_and(|s| s.success());
        if !made {
            eprintln!("skipping: no disk image could be made and attached here");
            return;
        }
        let src = t.file("a.bin", &[7u8; 256 * 1024]);
        let reader = File::open(&src).unwrap();
        let across = mount.join("a.bin");
        let cloned = clone_before_open(&reader, &across);
        let copied = crate::ops::copy::copy_tree(&src, &across, &TaskCtx::detached(), false);
        let read = std::fs::read(&across);
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&mount)
            .status();
        assert!(!cloned.unwrap(), "a clone across volumes");
        copied.unwrap();
        assert_eq!(read.unwrap(), vec![7u8; 256 * 1024]);
    }
}
