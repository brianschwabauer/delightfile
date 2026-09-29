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
//!
//! ## Phones and cameras
//!
//! A phone or a camera gvfs has mounted is a directory under gvfs-fuse's
//! (`$XDG_RUNTIME_DIR/gvfs/mtp:host=…`, `…/gphoto2:host=…`), and every read
//! of it is a USB round trip through the device's own protocol — slow, one at
//! a time, and on a phone held up whenever the screen locks. It is FUSE, so
//! [`is_remote`] would refuse it too, but only after asking: `statfs` on a
//! phone is itself a question the phone has to answer. [`on_device`] answers
//! from the path alone, so the size column and the grid's thumbnails can
//! leave a phone alone without touching it once.

use std::path::{Path, PathBuf};

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

/// Whether `path` sits on a filesystem an automatic walk should leave alone,
/// and the `statfs` magic that answers it: the platform's to ask
/// ([`crate::platform::fs::is_remote`]). Where it cannot be asked the answer is
/// "not remote", as it is for a path whose `statfs` failed.
pub use crate::platform::fs::{is_remote, magic_of};

/// Where gvfs-fuse shows gvfs's mounts: `$XDG_RUNTIME_DIR/gvfs`, which is
/// where gvfsd starts it, or `/run/user/<uid>/gvfs` when the variable is unset
/// or not an absolute path — the directory systemd would have given it.
pub fn gvfs_root() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", crate::ops::trash::uid())))
        .join("gvfs")
}

/// Whether `path` is inside a phone or a camera under the gvfs-fuse
/// directory `gvfs`: its first component below `gvfs` is an MTP or a gphoto2
/// mount (`mtp:host=…`, `gphoto2:host=…`). The gvfs directory itself, and a
/// share beside the phone (`sftp:host=…`), are not.
///
/// A question about the path, never about the disk: nothing here is read, so
/// the answer costs a phone nothing (see the module note).
pub fn on_device(path: &Path, gvfs: &Path) -> bool {
    let Ok(inside) = path.strip_prefix(gvfs) else {
        return false;
    };
    inside
        .components()
        .next()
        .and_then(|first| first.as_os_str().to_str())
        .is_some_and(|first| first.starts_with("mtp:") || first.starts_with("gphoto2:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path that is not there answers "not remote" rather than blocking the
    /// column on a failed syscall.
    #[test]
    fn an_unanswerable_path_is_not_remote() {
        assert!(!is_remote(Path::new("/nonexistent-df-fstype-probe")));
        assert_eq!(magic_of(Path::new("/nonexistent-df-fstype-probe")), None);
        // An embedded NUL cannot become a C string at all.
        assert_eq!(magic_of(Path::new("bad\0path")), None);
    }

    /// A phone or a camera is known by its gvfs-fuse directory, the device
    /// itself and anything inside it; the gvfs directory, a share beside it
    /// and a path somewhere else are not.
    #[test]
    fn a_phone_or_a_camera_is_known_by_its_path() {
        let gvfs = Path::new("/run/user/1000/gvfs");
        let phone = gvfs.join("mtp:host=Google_Pixel_10a_4B021FDAQ00123");
        assert!(on_device(&phone, gvfs));
        assert!(on_device(
            &phone.join("Internal shared storage/DCIM/Camera"),
            gvfs
        ));
        assert!(on_device(
            &gvfs.join("gphoto2:host=%5Busb%3A001%2C004%5D/store_00010001"),
            gvfs
        ));
        assert!(!on_device(gvfs, gvfs), "gvfs's own directory");
        assert!(!on_device(
            &gvfs.join("sftp:host=example.org,user=me"),
            gvfs
        ));
        assert!(!on_device(
            &gvfs.join("smb-share:server=nas,share=mtp:x"),
            gvfs
        ));
        assert!(!on_device(Path::new("/home/me/mtp:host=x"), gvfs));
        assert!(!on_device(
            Path::new("/run/user/1000/gvfs-other/mtp:host=x"),
            gvfs
        ));
    }

    /// The runtime directory the session gives gvfs, and the systemd one
    /// when it gives none.
    #[test]
    fn gvfs_lives_in_the_runtime_directory() {
        let root = gvfs_root();
        assert!(root.is_absolute());
        assert!(root.ends_with("gvfs"));
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
