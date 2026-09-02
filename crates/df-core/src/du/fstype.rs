//! What kind of filesystem a path is on, for the one decision that depends on
//! it: whether a recursive walk may start there on its own.
//!
//! The size column walks a directory the moment you enter it, and that is a
//! good bargain on a local disk — the metadata is in the page cache, the
//! workers are niced out of the way, and the answer is worth having. On a
//! network mount it is a different bargain entirely: every `stat` is a round
//! trip, a home directory over NFS is minutes of traffic nobody asked for, and
//! the walk holds the mount busy while somebody is trying to *browse* it.
//!
//! [`crate::du::DuOptions::cross_filesystems`] already keeps a walk from
//! *wandering* onto such a mount. What it cannot do is notice that the walk's
//! own root is one: an sshfs or NFS mount browsed as an ordinary path is an
//! ordinary path as far as `st_dev` is concerned, and the boundary check has
//! nothing to compare it against.
//!
//! So the root is asked directly, with one `statfs`. **This gates automatic
//! walks only.** `m u` — "what's big", which a person typed — walks whatever
//! they pointed it at: the cost is the answer they asked for, and refusing to
//! measure a mount because measuring it is slow would be the program deciding
//! it knows better.

use std::path::Path;

/// `f_type` values that mean "this is not local storage".
///
/// From `linux/magic.h` and the filesystems' own headers. Every FUSE mount is
/// in here as one number: `fuse.sshfs` and `fuse.rclone` are the cases this
/// exists for, and the local FUSE filesystems it also catches — `ntfs-3g`, a
/// gvfs mount of a phone — are ones a background walk has no business on
/// either, because every operation on them is a round trip through a userspace
/// daemon.
pub const REMOTE_FS_MAGIC: &[i64] = &[
    0x6969,      // NFS
    0xFF53_4D42, // CIFS / SMB1
    0xFE53_4D42, // SMB2
    0x517B,      // SMBFS
    0x6573_5546, // FUSE — sshfs, rclone, gvfs, ntfs-3g
    0x5346_414F, // AFS (OpenAFS)
    0x6B6C,      // AFS (kAFS)
    0x00C3_6400, // CephFS
    0x0102_1997, // 9P (v9fs)
    0x7461_636F, // OCFS2
    0x4711,      // Coda
    0x0BD0_0BD0, // Lustre
];

/// Whether `path` sits on a filesystem an automatic walk should leave alone.
///
/// `false` when the question cannot be answered — a path that has just been
/// deleted, a `statfs` that failed. Refusing to measure on a failed syscall
/// would turn one transient error into a column of em dashes.
pub fn is_remote(path: &Path) -> bool {
    magic_of(path).is_some_and(|magic| REMOTE_FS_MAGIC.contains(&magic))
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

    /// A path that is not there answers "not remote" rather than blocking the
    /// column on a failed syscall.
    #[test]
    fn an_unanswerable_path_is_not_remote() {
        assert!(!is_remote(Path::new("/nonexistent-df-fstype-probe")));
        assert_eq!(magic_of(Path::new("/nonexistent-df-fstype-probe")), None);
        // An embedded NUL cannot become a C string at all.
        assert_eq!(magic_of(Path::new("bad\0path")), None);
    }

    /// The table is the whole feature, so the three filesystems it exists for
    /// are asserted rather than trusted to a comment.
    #[test]
    fn the_table_holds_the_filesystems_this_is_about() {
        assert!(REMOTE_FS_MAGIC.contains(&0x6969), "NFS");
        assert!(REMOTE_FS_MAGIC.contains(&0x6573_5546), "FUSE");
        assert!(REMOTE_FS_MAGIC.contains(&0xFF53_4D42), "CIFS");
    }
}
