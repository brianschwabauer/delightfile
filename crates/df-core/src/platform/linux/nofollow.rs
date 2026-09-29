//! Finding a path without following a link: the Linux walk behind
//! [`crate::ops::mode`]'s permissions change.
//!
//! A change looks at a path and then sets its mode, and a path is a string
//! the kernel reads again each time: between the two, any folder on it can
//! be renamed and a link to somewhere else put in its place, and a `chmod`
//! by path would follow that link out of the tree — the same swap the trash
//! and the undo already close. So a change is asked in one folder, the
//! **anchor** — the listing's own folder, the one on screen, as the person
//! reached it, links and all — and nothing below it is reached by its path.
//! Each component is opened from the folder above it, open already, with
//! `O_PATH | O_NOFOLLOW` (and `O_DIRECTORY` for a folder), `fstat` says what
//! was opened, and the mode is set through `/proc/self/fd/<n>`, a name that
//! leads to that inode and no other. A row of a listing that is not a direct
//! child of its folder — a search hit three folders down — is found the same
//! way, all three folders looked up and none followed; a path that is not
//! below the anchor at all is refused.
//!
//! No `unsafe`: `open` with those flags is std's `OpenOptions` with
//! `custom_flags`, `fstat` is `File::metadata`, and `openat(dir, name)` is an
//! ordinary open of `/proc/self/fd/<dir>/<name>` — the kernel resolves the
//! descriptor's name to the folder it is open on, then looks `name` up there.
//! A descriptor opened with `O_PATH` cannot be `fchmod`ed (`EBADF`), and
//! `chmod` of its `/proc` name is the supported way to set its mode; it needs
//! no read permission, which a file of mode 000 being put right has not got.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ops::mode::{below, outside, MODE_BITS};

/// A folder the walk holds: an `O_PATH` descriptor.
pub type Folder = File;

/// A path the walk found, held to set its mode through: its own `O_PATH`
/// descriptor.
pub type Entry = File;

/// What a lookup answers: the inode's own metadata, read through
/// [`meta`] (the platform's, since it is a `Metadata`).
pub type Stat = std::fs::Metadata;

pub use crate::platform::meta;

/// Where the descriptors are named, and what the toast says without it.
const PROC_FD: &str = "/proc/self/fd";
const NO_PROC: &str =
    "/proc is not mounted, and without it permissions cannot be set without following links";

/// Whether descriptors can be named: `/proc` is mounted. Every change and
/// every undo asks this first, and refuses in words when it is not.
pub fn ready() -> std::result::Result<(), String> {
    ready_at(Path::new(PROC_FD))
}

fn ready_at(proc_fd: &Path) -> std::result::Result<(), String> {
    match std::fs::metadata(proc_fd) {
        Ok(meta) if meta.is_dir() => Ok(()),
        _ => Err(NO_PROC.to_string()),
    }
}

/// The name `file` has in `/proc`: a path to the inode it is open on,
/// whatever has been renamed since.
fn named(file: &File) -> PathBuf {
    Path::new(PROC_FD).join(file.as_raw_fd().to_string())
}

/// Open `path` only to name it (`O_PATH`), not following it if its last
/// component is a link — which then opens as the link, for the caller's
/// `fstat` to see. `dir` asks for a folder (`O_DIRECTORY`), which a link or
/// a file is not.
fn open_named(path: &Path, dir: bool) -> io::Result<File> {
    let mut flags = libc::O_PATH | libc::O_NOFOLLOW;
    if dir {
        flags |= libc::O_DIRECTORY;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
}

/// `name`, in the folder `dir` is open on: `openat`, through `/proc`.
pub fn open_in(dir: &File, name: &OsStr, want_dir: bool) -> io::Result<File> {
    open_named(&named(dir).join(name), want_dir)
}

/// `name`'s `lstat`, in the folder `dir` is open on: `fstatat` with
/// `AT_SYMLINK_NOFOLLOW`, through `/proc`.
pub fn stat_in(dir: &File, name: &OsStr) -> io::Result<std::fs::Metadata> {
    std::fs::symlink_metadata(named(dir).join(name))
}

/// A folder's listing, through its `/proc` name: the folder that descriptor is
/// open on, whatever has been renamed since.
pub fn read_dir_in(dir: &File) -> io::Result<std::fs::ReadDir> {
    std::fs::read_dir(named(dir))
}

/// Finds paths below an anchor: the anchor by its path, links followed,
/// since that is where the person is; then each component below it from
/// the one above, none followed.
///
/// Keeps the anchor and the last folder it found, the latter keyed by the
/// anchor **and** the path from it — never by the folder's path alone, since
/// one folder reached as an anchor (followed) and the same folder reached
/// from an anchor above it (not followed) are two different lookups. Paths
/// come a folder's worth at a time, so most lookups are one `open`.
#[derive(Default)]
pub struct Finder {
    anchor: Option<(PathBuf, Rc<File>)>,
    folder: Option<((PathBuf, PathBuf), Rc<File>)>,
}

impl Finder {
    /// The anchor, open, by its path.
    fn anchor(&mut self, anchor: &Path) -> io::Result<Rc<File>> {
        if let Some((path, dir)) = &self.anchor {
            if path == anchor {
                return Ok(Rc::clone(dir));
            }
        }
        let dir = Rc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
                .open(anchor)?,
        );
        self.anchor = Some((anchor.to_path_buf(), Rc::clone(&dir)));
        Ok(dir)
    }

    /// The folder `names` lead to from `anchor`, each looked up in the one
    /// before it and none followed.
    pub fn folder(&mut self, anchor: &Path, names: &[&OsStr]) -> io::Result<Rc<File>> {
        let key = (anchor.to_path_buf(), names.iter().collect::<PathBuf>());
        if let Some((cached, dir)) = &self.folder {
            if *cached == key {
                return Ok(Rc::clone(dir));
            }
        }
        let mut dir = self.anchor(anchor)?;
        for name in names {
            let next = open_in(&dir, name, true)?;
            if !next.metadata()?.is_dir() {
                return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
            }
            dir = Rc::new(next);
        }
        self.folder = Some((key, Rc::clone(&dir)));
        Ok(dir)
    }

    /// `path`, below `anchor`, opened to name it and not followed; what it
    /// is; and how many components below the anchor it is.
    pub fn open(
        &mut self,
        anchor: &Path,
        path: &Path,
        want_dir: bool,
    ) -> io::Result<(File, std::fs::Metadata, usize)> {
        let names = below(anchor, path)?;
        let (last, up) = names.split_last().ok_or_else(outside)?;
        let dir = self.folder(anchor, up)?;
        let file = open_in(&dir, last, want_dir)?;
        let meta = file.metadata()?;
        Ok((file, meta, names.len()))
    }
}

/// Whether an error says a folder on the way is not one any more: a link or
/// a file has taken its place.
pub fn swapped(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP))
}

/// Set the mode of the inode `file` is open on, through its `/proc` name.
pub fn set_mode_of(file: &File, mode: u32) -> std::result::Result<(), String> {
    match std::fs::set_permissions(
        named(file),
        std::fs::Permissions::from_mode(mode & MODE_BITS),
    ) {
        Ok(()) => Ok(()),
        // The descriptor is open, so its name not being there is `/proc`
        // having gone, which the caller checked for — and must not be
        // answered by trying the path instead.
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(NO_PROC.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// Without `/proc` there is no name for a descriptor, and the change
    /// says so rather than falling back to the path.
    #[test]
    fn without_proc_the_change_refuses_in_words() {
        if ready_at(Path::new(PROC_FD)).is_err() {
            eprintln!("skipped: /proc is not mounted here, which is the case this test describes");
            return;
        }
        let why = ready_at(Path::new("/nonexistent/df-proc")).unwrap_err();
        assert!(why.contains("/proc is not mounted"), "{why}");
    }
}
