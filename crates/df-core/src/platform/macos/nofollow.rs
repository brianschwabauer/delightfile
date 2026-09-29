//! Finding a path without following a link: the macOS walk behind
//! [`crate::ops::mode`]'s permissions change.
//!
//! The problem is Linux's (`platform/linux/nofollow.rs`): a change looks at a
//! path and then sets its mode, and any folder on the path can be swapped for
//! a link in between. The answer is the same shape — every component below
//! the **anchor** (the folder on screen, reached as the person reached it) is
//! looked up in the folder above it, open already, following nothing — but
//! macOS has neither `O_PATH` nor `/proc`, so it is made of the `*at` calls:
//!
//! - A folder on the way is `openat(folder, name, O_RDONLY | O_DIRECTORY |
//!   O_NOFOLLOW)`: a link where a folder was is `ELOOP`, a file `ENOTDIR`,
//!   both of which [`swapped`] reads. Without `O_PATH` a descriptor needs
//!   read permission, so a folder its owner has shut is opened only when it
//!   is used ([`Folder`] remembers where it is and which inode it was), and
//!   until then answers every use with the `EACCES` Linux would give — a
//!   listing refused is the "Apply again" case `ops::mode` reports.
//! - What the last component is comes from `fstatat(folder, name,
//!   AT_SYMLINK_NOFOLLOW)`, and its mode is set with `fchmodat(folder, name,
//!   mode, AT_SYMLINK_NOFOLLOW)`, which never follows a link. That needs no
//!   permission on the file itself, so a file of mode 000 is put right as on
//!   Linux. The folder is pinned by its descriptor; the name in it is looked
//!   up once for the `fstatat` and once for the `fchmodat`, and a file renamed
//!   over it between the two is caught by the inode the record keeps, which an
//!   undo checks before it touches anything.
//! - A listing is `fdopendir` of a duplicate of the folder's descriptor,
//!   rewound first, since a duplicate shares its offset with the original.
//!
//! What a lookup answers is a [`Stat`], this module's own copy of the three
//! numbers `ops::mode` reads (`std::fs::Metadata` cannot be made from an
//! `fstatat`), read through [`meta`] as Linux's `Metadata` is read through
//! `platform::meta`.
//!
//! The three rules of the other unsafe islands hold: every descriptor is
//! owned by exactly one `OwnedFd` or `DIR` that closes it once; every syscall's
//! return is checked and turned into [`io::Error::last_os_error`]; and nothing
//! unsafe escapes this file.

#![allow(unsafe_code)]

use std::cell::OnceCell;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::ops::mode::{below, outside, MODE_BITS};

/// Always ready: nothing here depends on a mounted `/proc`.
pub fn ready() -> std::result::Result<(), String> {
    Ok(())
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains a NUL byte"))
}

/// What a lookup found: the three numbers the permissions change reads, from
/// `fstatat`, of the name itself and never of a link's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    mode: u32,
    dev: u64,
    ino: u64,
}

/// A [`Stat`]'s kind, asked the way `std::fs::FileType` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileType {
    mode: u32,
}

impl FileType {
    pub fn is_symlink(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFLNK as u32
    }

    pub fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
    }
}

impl Stat {
    fn of(raw: &libc::stat) -> Stat {
        Stat {
            mode: u32::from(raw.st_mode),
            // As `std`'s `MetadataExt::dev` has it, so a record made here
            // and a `Metadata` read elsewhere agree.
            dev: raw.st_dev as u64,
            ino: raw.st_ino,
        }
    }

    pub fn file_type(&self) -> FileType {
        FileType { mode: self.mode }
    }

    pub fn is_dir(&self) -> bool {
        self.file_type().is_dir()
    }
}

/// The accessors for a [`Stat`], by the names `platform::meta` gives them for
/// a `Metadata`, so `ops::mode` reads what either platform found one way.
pub mod meta {
    use super::Stat;

    /// The whole `st_mode`: type and permission bits.
    pub fn mode(stat: &Stat) -> u32 {
        stat.mode
    }

    pub fn dev(stat: &Stat) -> u64 {
        stat.dev
    }

    pub fn ino(stat: &Stat) -> u64 {
        stat.ino
    }
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
fn stat_at(dir: RawFd, name: &OsStr) -> io::Result<Stat> {
    let c_name = c_name(name)?;
    let mut raw = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is a descriptor the caller keeps open for the call,
    // `c_name` is a NUL-terminated string that outlives it, and `raw` is a
    // correctly sized `stat` the kernel fills; it is read only on success.
    let rc = unsafe {
        libc::fstatat(
            dir,
            c_name.as_ptr(),
            raw.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a zero return means the kernel filled the struct.
    Ok(Stat::of(unsafe { raw.assume_init_ref() }))
}

/// `openat(dir, name, O_RDONLY | O_NOFOLLOW)`, and `O_DIRECTORY` for a folder.
fn open_at(dir: RawFd, name: &OsStr, want_dir: bool) -> io::Result<OwnedFd> {
    let c_name = c_name(name)?;
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if want_dir {
        flags |= libc::O_DIRECTORY;
    }
    // SAFETY: as `stat_at`: an open descriptor and a live C string.
    let fd = unsafe { libc::openat(dir, c_name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by `openat` and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A folder the walk holds, looked up without following it.
///
/// Opened when it is found, if its owner may read it. If not, it keeps the
/// folder it is in, its name there and the inode it was, and is opened when it
/// is next used — by then this very change may have opened it — and only if
/// the name still leads to that inode. Until it can be opened every use is
/// `EACCES`, the answer Linux's `O_PATH` descriptor gives a search of a folder
/// shut to its owner.
#[derive(Debug)]
pub struct Folder {
    fd: OnceCell<OwnedFd>,
    shut: Option<Shut>,
}

#[derive(Debug)]
struct Shut {
    parent: OwnedFd,
    name: OsString,
    inode: (u64, u64),
}

impl Folder {
    fn open(fd: OwnedFd) -> Folder {
        Folder {
            fd: OnceCell::from(fd),
            shut: None,
        }
    }

    /// The descriptor, opening a folder that was shut if it can be now.
    fn fd(&self) -> io::Result<RawFd> {
        if let Some(fd) = self.fd.get() {
            return Ok(fd.as_raw_fd());
        }
        let Some(shut) = &self.shut else {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        };
        let fd = open_at(shut.parent.as_raw_fd(), &shut.name, true)?;
        let now = File::from(fd.try_clone()?).metadata()?;
        {
            use std::os::unix::fs::MetadataExt;
            if (now.dev(), now.ino()) != shut.inode {
                // Another folder under the name: not the one that was found.
                return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
            }
        }
        Ok(self.fd.get_or_init(|| fd).as_raw_fd())
    }
}

/// `name`, in the folder `dir` is open on, opened without following it. A
/// folder (`want_dir`) its owner may not read yet is still found, and opened
/// when it is used (see [`Folder`]).
pub fn open_in(dir: &Folder, name: &OsStr, want_dir: bool) -> io::Result<Folder> {
    let parent = dir.fd()?;
    match open_at(parent, name, want_dir) {
        Ok(fd) => Ok(Folder::open(fd)),
        Err(e) if want_dir && e.raw_os_error() == Some(libc::EACCES) => {
            let stat = stat_at(parent, name)?;
            if stat.file_type().is_symlink() {
                return Err(io::Error::from_raw_os_error(libc::ELOOP));
            }
            if !stat.is_dir() {
                return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
            }
            // SAFETY: `parent` is open for the length of this call, and
            // `dup` returns a new descriptor that nothing else owns.
            let copy = unsafe { libc::dup(parent) };
            if copy < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: as above: `copy` is ours alone.
            let parent = unsafe { OwnedFd::from_raw_fd(copy) };
            Ok(Folder {
                fd: OnceCell::new(),
                shut: Some(Shut {
                    parent,
                    name: name.to_os_string(),
                    inode: (stat.dev, stat.ino),
                }),
            })
        }
        Err(e) => Err(e),
    }
}

/// `name`'s `lstat`, in the folder `dir` is open on: `fstatat` with
/// `AT_SYMLINK_NOFOLLOW`.
pub fn stat_in(dir: &Folder, name: &OsStr) -> io::Result<Stat> {
    stat_at(dir.fd()?, name)
}

/// One name in a listing.
#[derive(Debug)]
pub struct DirEntry {
    name: OsString,
}

impl DirEntry {
    pub fn file_name(&self) -> OsString {
        self.name.clone()
    }
}

/// A folder's listing, read from its own descriptor: the names in it, without
/// `.` and `..`.
#[derive(Debug)]
pub struct ReadDir {
    dir: std::ptr::NonNull<libc::DIR>,
}

impl Iterator for ReadDir {
    type Item = io::Result<DirEntry>;

    fn next(&mut self) -> Option<io::Result<DirEntry>> {
        loop {
            // SAFETY: `__error` is this thread's errno, cleared so that a NULL
            // from `readdir` can tell the end from a failure.
            unsafe { *libc::__error() = 0 };
            // SAFETY: `self.dir` is a live stream this struct owns.
            let entry = unsafe { libc::readdir(self.dir.as_ptr()) };
            if entry.is_null() {
                let e = io::Error::last_os_error();
                return (e.raw_os_error() != Some(0)).then_some(Err(e));
            }
            // SAFETY: a non-NULL `readdir` result points at an entry valid
            // until the next call on this stream, and `d_namlen` bytes of
            // `d_name` are the name.
            let name = unsafe {
                let raw = &*entry;
                let len = usize::from(raw.d_namlen).min(raw.d_name.len());
                std::slice::from_raw_parts(raw.d_name.as_ptr().cast::<u8>(), len).to_vec()
            };
            if name == b"." || name == b".." {
                continue;
            }
            return Some(Ok(DirEntry {
                name: OsString::from_vec(name),
            }));
        }
    }
}

impl Drop for ReadDir {
    fn drop(&mut self) {
        // SAFETY: the stream came from `fdopendir`, is owned solely here, and
        // drop runs once; closing it closes the duplicate it was made from.
        unsafe { libc::closedir(self.dir.as_ptr()) };
    }
}

/// A folder's listing, through its descriptor: the folder that descriptor is
/// open on, whatever has been renamed since. `EACCES` for one its owner may
/// not read.
pub fn read_dir_in(dir: &Folder) -> io::Result<ReadDir> {
    let fd = dir.fd()?;
    // SAFETY: `fd` is open for the call; `dup` returns a descriptor nothing
    // else owns, which `fdopendir` then takes.
    let copy = unsafe { libc::dup(fd) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `copy` is ours; on success the stream owns it, on failure it is
    // closed here.
    let stream = unsafe { libc::fdopendir(copy) };
    let Some(stream) = std::ptr::NonNull::new(stream) else {
        let e = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `copy` is still ours to close.
        unsafe { libc::close(copy) };
        return Err(e);
    };
    // SAFETY: a live stream. The duplicate shares its offset with the
    // folder's descriptor, which an earlier listing may have moved.
    unsafe { libc::rewinddir(stream.as_ptr()) };
    Ok(ReadDir { dir: stream })
}

/// A path found below an anchor, held as the folder it is in and its name
/// there — what its mode is set through.
#[derive(Debug)]
pub struct Entry {
    dir: Rc<Folder>,
    name: OsString,
}

impl Entry {
    /// What the name is now, not following it.
    pub fn metadata(&self) -> io::Result<Stat> {
        stat_in(&self.dir, &self.name)
    }
}

/// Finds paths below an anchor: the anchor by its path, links followed,
/// since that is where the person is; then each component below it from the
/// one above, none followed.
///
/// Keeps the anchor and the last folder it found, keyed as Linux's is: by
/// the anchor **and** the path from it, never by the folder's path alone.
#[derive(Default)]
pub struct Finder {
    anchor: Option<(PathBuf, Rc<Folder>)>,
    folder: Option<((PathBuf, PathBuf), Rc<Folder>)>,
}

impl Finder {
    /// The anchor, open, by its path.
    fn anchor(&mut self, anchor: &Path) -> io::Result<Rc<Folder>> {
        if let Some((path, dir)) = &self.anchor {
            if path == anchor {
                return Ok(Rc::clone(dir));
            }
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(anchor)?;
        let dir = Rc::new(Folder::open(OwnedFd::from(file)));
        self.anchor = Some((anchor.to_path_buf(), Rc::clone(&dir)));
        Ok(dir)
    }

    /// The folder `names` lead to from `anchor`, each looked up in the one
    /// before it and none followed.
    pub fn folder(&mut self, anchor: &Path, names: &[&OsStr]) -> io::Result<Rc<Folder>> {
        let key = (anchor.to_path_buf(), names.iter().collect::<PathBuf>());
        if let Some((cached, dir)) = &self.folder {
            if *cached == key {
                return Ok(Rc::clone(dir));
            }
        }
        let mut dir = self.anchor(anchor)?;
        for name in names {
            dir = Rc::new(open_in(&dir, name, true)?);
        }
        self.folder = Some((key, Rc::clone(&dir)));
        Ok(dir)
    }

    /// `path`, below `anchor`, found without following it; what it is; and
    /// how many components below the anchor it is. A folder asked for
    /// (`want_dir`) that is not one is `ENOTDIR`, as Linux's `O_DIRECTORY`
    /// open answers.
    pub fn open(
        &mut self,
        anchor: &Path,
        path: &Path,
        want_dir: bool,
    ) -> io::Result<(Entry, Stat, usize)> {
        let names = below(anchor, path)?;
        let (last, up) = names.split_last().ok_or_else(outside)?;
        let dir = self.folder(anchor, up)?;
        let stat = stat_in(&dir, last)?;
        if want_dir && !stat.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let entry = Entry {
            dir,
            name: last.to_os_string(),
        };
        Ok((entry, stat, names.len()))
    }
}

/// Whether an error says a folder on the way is not one any more: a link or
/// a file has taken its place.
pub fn swapped(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP))
}

/// Set the mode of what `entry` names, never following it: `fchmodat` with
/// `AT_SYMLINK_NOFOLLOW` in the folder it was found in.
pub fn set_mode_of(entry: &Entry, mode: u32) -> std::result::Result<(), String> {
    let set = || -> io::Result<()> {
        let dir = entry.dir.fd()?;
        let c_name = c_name(&entry.name)?;
        // SAFETY: an open descriptor and a live C string; the mode is a
        // scalar the kernel masks.
        let rc = unsafe {
            libc::fchmodat(
                dir,
                c_name.as_ptr(),
                (mode & MODE_BITS) as libc::mode_t,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    };
    set().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;
    use std::os::unix::fs::PermissionsExt;

    fn set(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A folder its owner has shut is found, refuses every use while shut,
    /// and is opened — the same folder, not a new one under its name — once
    /// it is open again.
    #[test]
    fn a_shut_folder_opens_when_it_is_next_used() {
        let t = TempTree::new("nofollow-shut");
        let shut = t.dir("shut");
        t.file("shut/inside.txt", b"i");
        set(&shut, 0);
        if std::fs::read_dir(&shut).is_ok() {
            eprintln!("skipped: this user can list a folder of mode 000");
            set(&shut, 0o755);
            return;
        }
        let mut finder = Finder::default();
        let top = finder.folder(t.path(), &[]).unwrap();
        let folder = open_in(&top, OsStr::new("shut"), true).unwrap();
        let refused = read_dir_in(&folder).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            stat_in(&folder, OsStr::new("inside.txt"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );

        set(&shut, 0o755);
        let names: Vec<OsString> = read_dir_in(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, [OsString::from("inside.txt")]);
        // Listed twice, whole both times: the offset is rewound.
        assert_eq!(read_dir_in(&folder).unwrap().count(), 1);

        // Shut again and replaced by another folder under the same name.
        set(&shut, 0);
        let other = open_in(&top, OsStr::new("shut"), true).unwrap();
        set(&shut, 0o755);
        std::fs::rename(&shut, t.join("moved")).unwrap();
        t.dir("shut");
        assert!(read_dir_in(&other).is_err_and(|e| swapped(&e)));
    }

    /// The last component is set where it is and never followed: a file of
    /// mode 000 is given one, and a link keeps its target as it was.
    #[test]
    fn a_mode_is_set_on_the_name_itself() {
        let t = TempTree::new("nofollow-set");
        let file = t.file("locked.txt", b"l");
        let outside = t.file("outside.txt", b"o");
        set(&file, 0);
        set(&outside, 0o644);
        let link = t.symlink(&outside, "link");
        let mut finder = Finder::default();

        let (entry, stat, depth) = finder.open(t.path(), &file, false).unwrap();
        assert_eq!((meta::mode(&stat) & 0o777, depth), (0, 1));
        set_mode_of(&entry, 0o640).unwrap();
        assert_eq!(meta::mode(&entry.metadata().unwrap()) & 0o777, 0o640);

        let (entry, stat, _) = finder.open(t.path(), &link, false).unwrap();
        assert!(stat.file_type().is_symlink());
        let _ = set_mode_of(&entry, 0o600);
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o644
        );
        // A folder asked for that is a link is not one.
        let asked = finder.open(t.path(), &link, true).map(|_| ());
        assert!(asked.is_err_and(|e| swapped(&e)));
    }
}
