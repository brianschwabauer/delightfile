//! The parts of a `stat` that `std::fs::Metadata` only offers through Unix's
//! `MetadataExt`: device and inode, link count, the raw `st_mode`, owner and
//! group, allocated blocks, the change time and the whole-second mtime.
//!
//! On Linux and macOS these are the kernel's own numbers, read straight off
//! the `stat` the caller already has. Every caller outside the platform asks
//! here, so a target whose metadata has no such fields (Windows) answers with
//! the closest thing it has rather than failing to compile.

use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

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
