//! The parts of a `stat` that `std::fs::Metadata` only offers through Unix's
//! `MetadataExt`: device and inode, link count, the raw `st_mode`, owner and
//! group, allocated blocks, the change time and the whole-second mtime.
//!
//! On Linux and macOS these are the kernel's own numbers, read straight off
//! the `stat` the caller already has. Every caller outside the platform asks
//! here, so a target whose metadata has no such fields (Windows) answers with
//! the closest thing it has rather than failing to compile.
//!
//! [`identity`] and [`maybe_linked`] take the path as well as the `stat`
//! because Windows keeps a file's identity in a handle, not in its metadata;
//! here the `stat` has it all, and the path is not touched.

use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// What names one file on one device, and how many names it has: two paths
/// with the same `dev` and `ino` are the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identity {
    /// `st_dev`.
    pub dev: u64,
    /// `st_ino`.
    pub ino: u64,
    /// `st_nlink`.
    pub nlink: u64,
}

/// Device, inode and link count of the file `meta` describes — `path`'s
/// `symlink_metadata`, so a link is itself. Read off `meta`; never an error
/// here.
pub fn identity(_path: &Path, meta: &Metadata) -> io::Result<Identity> {
    Ok(Identity {
        dev: meta.dev(),
        ino: meta.ino(),
        nlink: meta.nlink(),
    })
}

/// Whether the file has names besides this one (`st_nlink > 1`): the files
/// the du walk counts once.
pub fn maybe_linked(meta: &Metadata) -> bool {
    meta.nlink() > 1
}

/// Hidden by the Unix convention, and only by it: a leading dot. (macOS's
/// `UF_HIDDEN` flag is not read — it hides `~/Library`, which a file manager
/// for people who want to see everything should show.)
pub fn is_hidden(name: &OsStr, _meta: &Metadata) -> bool {
    name.as_bytes().first() == Some(&b'.')
}

/// The device the file is on (`st_dev`).
pub fn dev(meta: &Metadata) -> u64 {
    meta.dev()
}

/// The inode number (`st_ino`).
pub fn ino(meta: &Metadata) -> u64 {
    meta.ino()
}

/// How many names the inode has (`st_nlink`).
pub fn nlink(meta: &Metadata) -> u64 {
    meta.nlink()
}

/// The raw `st_mode`: file-type bits and permission bits together.
pub fn mode(meta: &Metadata) -> u32 {
    meta.mode()
}

/// The owner (`st_uid`).
pub fn uid(meta: &Metadata) -> u32 {
    meta.uid()
}

/// The group (`st_gid`).
pub fn gid(meta: &Metadata) -> u32 {
    meta.gid()
}

/// The bytes the file occupies on disk: `st_blocks` in the 512-byte unit
/// every Unix reports it in ([`crate::du::BLOCK_UNIT`]).
pub fn blocks_bytes(meta: &Metadata) -> u64 {
    meta.blocks().saturating_mul(crate::du::BLOCK_UNIT)
}

/// The change time (`st_ctime`, `st_ctime_nsec`): when the inode last
/// changed, which `std` has no accessor for.
pub fn change_time(meta: &Metadata) -> (i64, i64) {
    (meta.ctime(), meta.ctime_nsec())
}

/// The modification time in whole seconds since the epoch (`st_mtime`),
/// negative before 1970.
pub fn mtime(meta: &Metadata) -> i64 {
    meta.mtime()
}
