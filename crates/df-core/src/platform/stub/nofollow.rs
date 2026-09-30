//! No way to find a path without following a link: stands in on Windows for
//! good, where permissions editing does not exist (`04-windows.md` Decisions
//! log). macOS walks with the `*at` calls (`platform/macos/nofollow.rs`).
//!
//! [`ready`] refuses, and every change and undo in [`crate::ops::mode`] asks it
//! first, so the permissions change reports "Permissions is not available on
//! this platform" for each path and touches nothing. It never falls back to
//! setting a mode by path, which would follow a link swapped into the tree.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::Path;
use std::rc::Rc;

use crate::DfError;

/// A folder the walk would hold; never made.
pub type Folder = File;

/// A path the walk would find; never made.
pub type Entry = File;

/// What a lookup would answer, read through [`meta`].
pub type Stat = std::fs::Metadata;

pub use crate::platform::meta;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        DfError::Unsupported("Permissions"),
    )
}

/// Always refused, in words.
pub fn ready() -> std::result::Result<(), String> {
    Err(DfError::Unsupported("Permissions").to_string())
}

/// Refused.
pub fn open_in(_dir: &File, _name: &OsStr, _want_dir: bool) -> io::Result<File> {
    Err(unsupported())
}

/// Refused.
pub fn stat_in(_dir: &File, _name: &OsStr) -> io::Result<std::fs::Metadata> {
    Err(unsupported())
}

/// Refused.
pub fn read_dir_in(_dir: &File) -> io::Result<std::fs::ReadDir> {
    Err(unsupported())
}

/// Finds nothing: every lookup is refused.
///
/// A struct with a field, though it holds nothing, because `ops::mode` makes
/// one with `Finder::default()` as it does Linux's, and clippy objects to
/// `default()` on a unit struct (`default_constructed_unit_structs`).
#[derive(Default)]
pub struct Finder {
    _nothing: (),
}

impl Finder {
    /// Refused.
    pub fn folder(&mut self, _anchor: &Path, _names: &[&OsStr]) -> io::Result<Rc<File>> {
        Err(unsupported())
    }

    /// Refused.
    pub fn open(
        &mut self,
        _anchor: &Path,
        _path: &Path,
        _want_dir: bool,
    ) -> io::Result<(File, std::fs::Metadata, usize)> {
        Err(unsupported())
    }
}

/// Never: nothing here looks a path up to find it swapped.
pub fn swapped(_e: &io::Error) -> bool {
    false
}

/// Refused, in words.
pub fn set_mode_of(_file: &File, _mode: u32) -> std::result::Result<(), String> {
    Err(DfError::Unsupported("Permissions").to_string())
}
