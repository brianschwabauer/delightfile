//! The Linux-only file primitives, answered with "no": stands in on macOS and
//! on Windows.
//!
//! - [`reflink`]: `false`, so every copy takes the chunked path. macOS gets
//!   `clonefile` as a pre-open hook in M2.2; Windows keeps the chunked copy.
//! - [`forget_cached`]: nothing. A verify then reads through the cache, which
//!   still catches everything but a lying medium; macOS stays this way (M2.7).
//! - [`is_remote`] / [`magic_of`]: `false` / `None`, so automatic folder sizes
//!   walk a network mount as they would a disk until M2.3 and W4.x answer it.

use std::fs::File;
use std::path::Path;

/// Never: no reflink on this platform (yet).
pub fn reflink(_reader: &File, _writer: &File) -> bool {
    false
}

/// Nothing to ask the kernel for here.
pub fn forget_cached(_file: &File, _path: &Path) {}

/// Not known, so not remote.
pub fn is_remote(_path: &Path) -> bool {
    false
}

/// No `statfs` magic on this platform.
pub fn magic_of(_path: &Path) -> Option<i64> {
    None
}
