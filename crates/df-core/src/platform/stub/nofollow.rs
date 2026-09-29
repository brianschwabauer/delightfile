//! No way to find a path without following a link: stands in on macOS until
//! the `openat`/`O_NOFOLLOW` walk lands (M2.28) and on Windows, where whether
//! permissions editing exists at all is an open question (`04-windows.md`).
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
#[derive(Default)]
pub struct Finder;

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
