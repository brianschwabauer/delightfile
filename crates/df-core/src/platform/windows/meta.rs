//! What Windows has of a Unix `stat`, enough to compile against and to draw
//! sensible rows, until W4.5 reads the real numbers.
//!
//! `std`'s stable Windows metadata has attributes, three times and a size; the
//! volume serial, file index and link count are behind the unstable
//! `windows_by_handle`, and W4.5 gets them from `GetFileInformationByHandle`
//! instead. Until then: no device or inode (every file looks like every other
//! to the du hard-link dedupe, which then counts each name once, and to a
//! boundary check, which sees one device), one link, a mode made up from the
//! file type and the read-only flag, no owner, the size for the blocks, and
//! the last write for the change time.

use std::fs::Metadata;
use std::time::SystemTime;

const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;

/// No device number yet (W4.5: the volume serial).
pub fn dev(_meta: &Metadata) -> u64 {
    0
}

/// No inode number yet (W4.5: the file index).
pub fn ino(_meta: &Metadata) -> u64 {
    0
}

/// One name, until W4.5 reads `nNumberOfLinks`.
pub fn nlink(_meta: &Metadata) -> u64 {
    1
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

/// The size, for want of the allocation W4.5 reads with
/// `GetCompressedFileSizeW`.
pub fn blocks_bytes(meta: &Metadata) -> u64 {
    meta.len()
}

/// The last write, as seconds and nanoseconds since the epoch: Windows keeps
/// no inode change time.
pub fn change_time(meta: &Metadata) -> (i64, i64) {
    let Ok(modified) = meta.modified() else {
        return (0, 0);
    };
    match modified.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i64, i64::from(d.subsec_nanos())),
        Err(e) => {
            let d = e.duration();
            (-(d.as_secs() as i64), -i64::from(d.subsec_nanos()))
        }
    }
}

/// The last write in whole seconds since the epoch, negative before 1970.
pub fn mtime(meta: &Metadata) -> i64 {
    change_time(meta).0
}
