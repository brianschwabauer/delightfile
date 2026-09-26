//! Permissions as an operation: the `C` card's nine bits set on a selection,
//! or on everything inside one, and `u` putting them back.
//!
//! ## Nine bits, three states
//!
//! A selection's bits are not one number. A 644 file and a 600 file agree
//! about the owner and disagree about the group and everybody else, so what
//! the card edits is a [`Grid`] of nine [`Cell`]s, each on, off or
//! [`Cell::Mixed`]. A cell left mixed is left alone: each item keeps its own
//! bit there ([`Grid::apply`]).
//!
//! Only the nine are the card's. Setuid, setgid and sticky are never shown,
//! and every change carries them through as they were. A permissions editor
//! that could clear setuid on a program by ticking a read box would be a way
//! to break a system by accident.
//!
//! ## Everything inside: the `X` rule
//!
//! Applied to a folder and everything in it, the grid is what the **files**
//! get. Every folder gets the grid plus execute wherever it grants read
//! ([`searchable`]), the convention `chmod -R u=rwX,go=rX` spells: a folder
//! that can be read and not entered lists names and opens none of them, and
//! "make all of this 644" never meant that for the folders. The folder the
//! change was asked of is one of them, or the change would lock its owner out
//! of what it had just changed.
//!
//! ## Links
//!
//! `chmod(2)` follows a symlink and Linux has no `lchmod`, so setting a
//! link's mode sets its target's: a file nobody selected, possibly nowhere
//! near here. A link is skipped wherever it is met, in the selection or in a
//! tree, and never followed — including one put where a folder was between
//! the plan and the change, or the change and its undo. Nothing below the
//! folder the change was asked in (the listing's, however far below it a row
//! is) is reached by its path: each component is opened from the one above
//! it without following it, and the mode is set on what was opened (see
//! "Finding a path without following a link" below).
//!
//! ## Order
//!
//! A folder that loses its execute bit cannot be searched any more, so
//! nothing under it can be changed once it has been; one that gains it can
//! only be searched once it has. [`chmod`] sets the folders that end up
//! searchable first, from the top down, and everything else from the bottom
//! up ([`safe_order`]), and the undo restores in the same order for the modes
//! it is putting back.
//!
//! ## Undo
//!
//! [`OpRecord::Mode`] keeps each path's mode before and after, the after read
//! back from the disk rather than assumed: the kernel quietly drops setgid on
//! a file whose group the caller is not in. It keeps the inode too, so a file
//! deleted and made again is not mistaken for the one that was changed. The
//! inverse checks that every path is still that file with exactly the mode
//! the change left before it restores any of them, the journal's
//! refusal-before-touching rule, and a forward re-apply is the same walk with
//! `before` and `after` swapped.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::tasks::TaskCtx;
use crate::text::grouped;
use crate::{DfError, Result};

use super::journal::{OpRecord, UndoAttempt, UndoReport, MAX_MANIFEST_ENTRIES};

/// The nine permission bits: owner, group and everybody else, read, write
/// and execute.
pub const PERMISSIONS: u32 = 0o777;

/// Setuid, setgid and sticky: never shown, never changed.
pub const SPECIAL: u32 = 0o7000;

/// Every bit `chmod(2)` sets, which is what a record compares.
pub const MODE_BITS: u32 = 0o7777;

/// The nine bits in the order `ls` writes them, which is the grid's order:
/// the owner's read, write and execute, then the group's, then everybody
/// else's.
pub const BITS: [u32; 9] = [
    0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001,
];

/// The bit that makes a folder searchable by its owner, which is who changes
/// its mode: what [`safe_order`] reads to decide whether what is under a
/// folder can be reached once the folder has its new mode.
const OWNER_SEARCH: u32 = 0o100;

/// One of the nine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Off,
    On,
    /// Set on some of the items and not on others. Left alone, it stays each
    /// item's own.
    Mixed,
}

/// A selection's nine bits, each on, off or mixed, in [`BITS`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cells: [Cell; 9],
}

impl Grid {
    /// One mode's nine bits, none of them mixed.
    pub fn of_mode(mode: u32) -> Grid {
        Grid::of([mode])
    }

    /// What `modes` have in common: a bit on in all of them is on, one on in
    /// none is off, and the rest are mixed. No modes at all is all off.
    pub fn of(modes: impl IntoIterator<Item = u32>) -> Grid {
        let mut every = PERMISSIONS;
        let mut any = 0;
        let mut seen = false;
        for mode in modes {
            every &= mode;
            any |= mode;
            seen = true;
        }
        if !seen {
            every = 0;
        }
        let mut cells = [Cell::Off; 9];
        for (cell, bit) in cells.iter_mut().zip(BITS) {
            *cell = if every & bit != 0 {
                Cell::On
            } else if any & bit != 0 {
                Cell::Mixed
            } else {
                Cell::Off
            };
        }
        Grid { cells }
    }

    /// Three or four octal digits (`755`, `0755`) as a grid with nothing
    /// mixed, or `None` for anything else. A fourth digit is the special
    /// bits', which the grid does not hold: it is accepted, because `0755` is
    /// how a great many people write 755, and it changes nothing.
    pub fn parse(text: &str) -> Option<Grid> {
        let digits = text.trim();
        if !(3..=4).contains(&digits.len()) || !digits.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return None;
        }
        let value = u32::from_str_radix(digits, 8).ok()?;
        Some(Grid::of_mode(value & PERMISSIONS))
    }

    /// The grid as three octal digits, `755`. A digit with a mixed cell in
    /// its triad has no value to write and is a dash, `–55`, the grid's own
    /// mark for mixed.
    pub fn octal(&self) -> String {
        (0..3)
            .map(|who| &self.cells[who * 3..who * 3 + 3])
            .map(|triad| {
                if triad.contains(&Cell::Mixed) {
                    return '–';
                }
                let digit = triad
                    .iter()
                    .zip([4, 2, 1])
                    .filter(|(cell, _)| **cell == Cell::On)
                    .map(|(_, value)| value)
                    .sum::<u32>();
                char::from_digit(digit, 8).unwrap_or('0')
            })
            .collect()
    }

    /// The nine bits as one mode, when none of them is mixed.
    pub fn mode(&self) -> Option<u32> {
        if self.is_mixed() {
            return None;
        }
        Some(
            self.cells
                .iter()
                .zip(BITS)
                .filter(|(cell, _)| **cell == Cell::On)
                .fold(0, |mode, (_, bit)| mode | bit),
        )
    }

    pub fn is_mixed(&self) -> bool {
        self.cells.contains(&Cell::Mixed)
    }

    /// Flip one cell. A mixed cell becomes on, as a mixed checkbox does
    /// everywhere: the press is a decision, and the first one is "all of
    /// them".
    pub fn toggle(&mut self, index: usize) {
        if let Some(cell) = self.cells.get_mut(index) {
            *cell = match cell {
                Cell::On => Cell::Off,
                Cell::Off | Cell::Mixed => Cell::On,
            };
        }
    }

    /// `mode` with the grid's bits in it: on and off where the grid says so,
    /// the item's own bit where the grid is mixed, and everything outside the
    /// nine (the special bits, the file type) exactly as it was.
    pub fn apply(&self, mode: u32) -> u32 {
        let mut out = mode & !PERMISSIONS;
        for (cell, bit) in self.cells.iter().zip(BITS) {
            let on = match cell {
                Cell::On => true,
                Cell::Off => false,
                Cell::Mixed => mode & bit != 0,
            };
            if on {
                out |= bit;
            }
        }
        out
    }
}

/// `mode` plus execute wherever it grants read: the `X` rule a folder gets
/// when a change goes inside it (see the module header).
pub fn searchable(mode: u32) -> u32 {
    mode | ((mode & 0o444) >> 2)
}

/// One path's mode before and after a change: the whole `st_mode` each time,
/// file type and all, which inode it was, and how to find the path again.
///
/// The whole mode rather than the twelve bits `chmod` sets, for two reasons.
/// Comparing it notices a file that was replaced by a folder of the same name
/// as well as one whose bits moved. And it says which paths are folders
/// without asking the disk, which the undo needs for a folder the change
/// shut: nothing under it can be asked anything until it is open again.
///
/// The inode (`dev`, `ino`) is the file's identity: a file deleted and made
/// again with the same mode is a different file, and neither an undo nor a
/// redo may give it a mode that was meant for the one before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChange {
    pub path: PathBuf,
    pub before: u32,
    pub after: u32,
    /// How many of `path`'s last components are below the folder the change
    /// was asked in: 1 for a row of it, more for a row further down or for
    /// what a change found inside a folder. The undo looks each of them up
    /// from that folder without following a link, as the change did.
    pub depth: usize,
    pub dev: u64,
    pub ino: u64,
}

impl ModeChange {
    /// Whether the path is a folder, by the file type the record kept.
    fn is_dir(&self) -> bool {
        self.after & FILE_TYPE == DIRECTORY
    }

    /// The folder the change was asked in: `path` less its last `depth`
    /// components.
    fn anchor(&self) -> io::Result<PathBuf> {
        let components: Vec<Component<'_>> = self.path.components().collect();
        if self.depth == 0 || self.depth >= components.len() {
            return Err(outside());
        }
        Ok(components[..components.len() - self.depth].iter().collect())
    }
}

/// `st_mode`'s file-type field, and its value for a folder.
const FILE_TYPE: u32 = 0o170000;
const DIRECTORY: u32 = 0o040000;

/// One path a change will set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// Somewhere below the folder the change is asked in ([`chmod`]'s
    /// `anchor`), which is what it is looked up from.
    pub path: PathBuf,
    /// The mode to set, with the file type the plan found at the path: the
    /// kind of file it must still be when it is set. With no file type in it,
    /// any kind will do but a link.
    pub mode: u32,
}

/// What a change is about to do, worked out before any of it is done.
#[derive(Debug, Default)]
pub struct ModePlan {
    /// What [`chmod`] sets: each mode the grid applied to that path's own,
    /// parents before what is in them.
    pub pairs: Vec<Planned>,
    /// Links met and left alone.
    pub links: usize,
    /// Targets, or entries of a folder being walked, that were gone by the
    /// time the plan looked.
    pub gone: usize,
    /// Folders that could not be listed because the change itself is what
    /// will open them: a folder of mode 000 given one its owner can read and
    /// enter. A second Apply reaches inside.
    pub again: Vec<PathBuf>,
    /// Paths that could not be read: the item itself, or a folder the walk
    /// could not list.
    pub errors: Vec<(PathBuf, String)>,
}

// ── Finding a path without following a link ────────────────────────────────
//
// A change looks at a path and then sets its mode, and a path is a string
// the kernel reads again each time: between the two, any folder on it can
// be renamed and a link to somewhere else put in its place, and a `chmod`
// by path would follow that link out of the tree — the same swap the trash
// and the undo already close. So a change is asked in one folder, the
// **anchor** — the listing's own folder, the one on screen, as the person
// reached it, links and all — and nothing below it is reached by its path.
// Each component is opened from the folder above it, open already, with
// `O_PATH | O_NOFOLLOW` (and `O_DIRECTORY` for a folder), `fstat` says what
// was opened, and the mode is set through `/proc/self/fd/<n>`, a name that
// leads to that inode and no other. A row of a listing that is not a direct
// child of its folder — a search hit three folders down — is found the same
// way, all three folders looked up and none followed; a path that is not
// below the anchor at all is refused.
//
// No `unsafe`: `open` with those flags is std's `OpenOptions` with
// `custom_flags`, `fstat` is `File::metadata`, and `openat(dir, name)` is an
// ordinary open of `/proc/self/fd/<dir>/<name>` — the kernel resolves the
// descriptor's name to the folder it is open on, then looks `name` up there.
// A descriptor opened with `O_PATH` cannot be `fchmod`ed (`EBADF`), and
// `chmod` of its `/proc` name is the supported way to set its mode; it needs
// no read permission, which a file of mode 000 being put right has not got.

/// Where the descriptors are named, and what the toast says without it.
const PROC_FD: &str = "/proc/self/fd";
const NO_PROC: &str =
    "/proc is not mounted, and without it permissions cannot be set without following links";

/// What a path outside the anchor, or one that is the anchor itself, says.
const NOT_BELOW: &str = "not inside the folder the change was asked in";

/// Whether descriptors can be named: `/proc` is mounted.
fn proc_ready() -> std::result::Result<(), String> {
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
fn open_in(dir: &File, name: &OsStr, want_dir: bool) -> io::Result<File> {
    open_named(&named(dir).join(name), want_dir)
}

/// `name`'s `lstat`, in the folder `dir` is open on: `fstatat` with
/// `AT_SYMLINK_NOFOLLOW`, through `/proc`.
fn stat_in(dir: &File, name: &OsStr) -> io::Result<std::fs::Metadata> {
    std::fs::symlink_metadata(named(dir).join(name))
}

fn outside() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, NOT_BELOW)
}

/// The names that lead from `anchor` down to `path`, one per component, or
/// [`NOT_BELOW`] when `path` is not strictly below it. Compared component by
/// component, so `/a/bc` is not below `/a/b`, and a `..` anywhere below the
/// anchor is refused rather than walked.
fn below<'a>(anchor: &Path, path: &'a Path) -> io::Result<Vec<&'a OsStr>> {
    let rest = path.strip_prefix(anchor).map_err(|_| outside())?;
    let names = rest
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(outside()),
        })
        .collect::<io::Result<Vec<&OsStr>>>()?;
    if names.is_empty() {
        return Err(outside());
    }
    Ok(names)
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
struct Finder {
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
    fn folder(&mut self, anchor: &Path, names: &[&OsStr]) -> io::Result<Rc<File>> {
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
    fn open(
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
fn swapped(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP))
}

/// What a lookup that failed says, for a toast that names the path itself.
fn lookup_failed(e: &io::Error) -> String {
    if swapped(e) {
        "not where it was — a link or a file is in the way, and it was left alone".to_string()
    } else {
        e.to_string()
    }
}

/// What a kind that changed says, for the same toast.
const NOT_THE_SAME_KIND: &str = "not the same kind of file any more, and it was left alone";

/// Set the mode of the inode `file` is open on, through its `/proc` name.
fn set_mode_of(file: &File, mode: u32) -> std::result::Result<(), String> {
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

/// Work out the new mode of every path the change is about: each of
/// `targets`, and with `recursive`, everything inside the folders among them
/// ([`searchable`] for every folder). Links are counted and skipped, and so
/// are targets already gone.
///
/// `anchor` is the folder the change is asked in — the listing's own — and
/// every target has to be below it: one that is not is refused, with an
/// error, rather than looked up some other way. Each target is found from the
/// anchor without following anything below it, and the walk goes down by
/// descriptors (see "Finding a path without following a link" above), so a
/// folder renamed or swapped for a link after it was seen cannot send the walk
/// anywhere else. It walks with an explicit stack, so a deep tree is a long
/// `Vec` and not a stack overflow, and each pending folder holds only the
/// folder it is in open, so what is open at once is the folders on the way
/// down rather than every folder met. It asks `ctx` between entries: the only
/// error is [`DfError::Cancelled`], and everything it cannot read is in
/// [`ModePlan::errors`] — or, for a folder only this change will open, in
/// [`ModePlan::again`].
pub fn plan(
    anchor: &Path,
    targets: &[PathBuf],
    grid: &Grid,
    recursive: bool,
    ctx: &TaskCtx,
) -> Result<ModePlan> {
    let mut plan = ModePlan::default();
    if let Err(why) = proc_ready() {
        plan.errors = targets.iter().map(|t| (t.clone(), why.clone())).collect();
        return Ok(plan);
    }
    // Folders still to be listed: the folder each is in, open, its path for
    // the record — never the path to open — and the mode it is to be given.
    let mut folders: Vec<(Rc<File>, PathBuf, u32)> = Vec::new();
    let mut finder = Finder::default();
    for target in targets {
        ctx.checkpoint()?;
        let looked = below(anchor, target).and_then(|names| {
            let (last, up) = names.split_last().ok_or_else(outside)?;
            let dir = finder.folder(anchor, up)?;
            let meta = stat_in(&dir, last)?;
            Ok((dir, meta))
        });
        match looked {
            Err(e) if e.kind() == io::ErrorKind::NotFound => plan.gone += 1,
            Err(e) => plan.errors.push((target.clone(), lookup_failed(&e))),
            Ok((_, meta)) if meta.file_type().is_symlink() => plan.links += 1,
            Ok((dir, meta)) if recursive && meta.is_dir() => {
                let mode = searchable(grid.apply(meta.mode()));
                plan.pairs.push(Planned {
                    path: target.clone(),
                    mode,
                });
                folders.push((dir, target.clone(), mode));
            }
            Ok((_, meta)) => plan.pairs.push(Planned {
                path: target.clone(),
                mode: grid.apply(meta.mode()),
            }),
        }
    }
    while let Some((parent, folder, mode)) = folders.pop() {
        let opened = folder
            .file_name()
            .ok_or_else(outside)
            .and_then(|name| open_in(&parent, name, true));
        drop(parent);
        let dir = match opened {
            Ok(dir) => Rc::new(dir),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                plan.gone += 1;
                continue;
            }
            Err(e) => {
                plan.errors.push((folder, lookup_failed(&e)));
                continue;
            }
        };
        let listing = match std::fs::read_dir(named(&dir)) {
            Ok(listing) => listing,
            // Shut to its owner now, and opened to them by this very change:
            // the second Apply lists it. Said as that, not as an error.
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied
                    && mode & OWNER_READ_SEARCH == OWNER_READ_SEARCH =>
            {
                plan.again.push(folder);
                continue;
            }
            Err(e) => {
                plan.errors.push((folder, e.to_string()));
                continue;
            }
        };
        for entry in listing {
            ctx.checkpoint()?;
            let name = match entry {
                Ok(entry) => entry.file_name(),
                Err(e) => {
                    plan.errors.push((folder.clone(), e.to_string()));
                    continue;
                }
            };
            let path = folder.join(&name);
            match stat_in(&dir, &name) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => plan.gone += 1,
                Err(e) => plan.errors.push((path, e.to_string())),
                Ok(meta) if meta.file_type().is_symlink() => plan.links += 1,
                Ok(meta) if meta.is_dir() => {
                    let mode = searchable(grid.apply(meta.mode()));
                    plan.pairs.push(Planned {
                        path: path.clone(),
                        mode,
                    });
                    folders.push((Rc::clone(&dir), path, mode));
                }
                Ok(meta) => plan.pairs.push(Planned {
                    path,
                    mode: grid.apply(meta.mode()),
                }),
            }
        }
    }
    Ok(plan)
}

/// The owner's read and execute: what a folder needs for its owner to list
/// what is in it.
const OWNER_READ_SEARCH: u32 = 0o500;

/// What [`chmod`] did — and, once [`ModeReport::absorb`] has taken it in,
/// what the plan before it found.
#[derive(Debug, Default)]
pub struct ModeReport {
    /// The inverse, when anything changed and the change was small enough to
    /// record ([`MAX_MANIFEST_ENTRIES`]).
    pub record: Option<OpRecord>,
    /// Paths whose mode is different now.
    pub changed: usize,
    /// Paths that already had the mode they were given.
    pub unchanged: usize,
    /// Links met and left alone.
    pub links: usize,
    /// Planned paths that were gone when their turn came: counted, not
    /// reported one by one. Something else deleted them, and there is
    /// nothing to do about it.
    pub gone: usize,
    /// Folders a second Apply will reach inside ([`ModePlan::again`]).
    pub again: Vec<PathBuf>,
    /// More changed than a record holds: done, and not undoable.
    pub unrecorded: bool,
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

impl ModeReport {
    /// Take in what the plan found, so one report says the whole of it.
    pub fn absorb(&mut self, plan: ModePlan) {
        self.links += plan.links;
        self.gone += plan.gone;
        self.again = plan.again;
        let mut errors = plan.errors;
        errors.append(&mut self.errors);
        self.errors = errors;
    }

    /// The toast's line: what changed, and then what did not and why, each
    /// said once however many paths it covers. Failures are the toast's to
    /// add (`op_toast` counts them), since they name a file.
    pub fn message(&self) -> String {
        let mut line = if self.changed == 0 {
            "Permissions unchanged".to_string()
        } else {
            format!(
                "Permissions set on {}",
                plural(self.changed, "item", "items")
            )
        };
        if self.unrecorded {
            line.push_str(" · too many to undo");
        }
        if self.gone > 0 {
            line.push_str(&format!(" · {} no longer there", grouped(self.gone as u64)));
        }
        match self.again.as_slice() {
            [] => {}
            [one] => line.push_str(&format!(
                " · Apply again to reach inside {}",
                one.file_name().unwrap_or_default().to_string_lossy()
            )),
            many => line.push_str(&format!(
                " · Apply again to reach inside {} folders",
                grouped(many.len() as u64)
            )),
        }
        line
    }
}

/// Set each path's mode, in the order that keeps every folder searchable
/// until what is under it has been set ([`safe_order`]).
///
/// `anchor` is the folder the change was asked in, and every path is found
/// from it: each component below it opened from the one above, following no
/// link, then `fstat`ed. A path not below the anchor is refused; one that is
/// no longer the kind of file the plan recorded, or that has a link or a
/// file where a folder on the way was, is left alone and reported; one that
/// is gone is counted as gone. The nine bits come from the pair; the special
/// bits are kept as the inode has them now, whatever the pair says. A link is
/// skipped, and a path that already has its mode is left untouched and not
/// recorded. Failures are collected rather than returned, and a cancel stops
/// between two paths with everything set so far recorded, so a `u` takes
/// back exactly the part that happened.
pub fn chmod(anchor: &Path, pairs: &[Planned], ctx: &TaskCtx) -> ModeReport {
    chmod_capped(anchor, pairs, ctx, MAX_MANIFEST_ENTRIES)
}

/// [`chmod`] with the record's bound spelled out, so the bound is testable
/// without fifty thousand files.
fn chmod_capped(anchor: &Path, pairs: &[Planned], ctx: &TaskCtx, cap: usize) -> ModeReport {
    let mut report = ModeReport::default();
    if let Err(why) = proc_ready() {
        report.errors = pairs
            .iter()
            .map(|p| (p.path.clone(), why.clone()))
            .collect();
        return report;
    }
    let order = safe_order(
        &pairs
            .iter()
            .map(|pair| {
                (
                    pair.path.as_path(),
                    pair.mode & FILE_TYPE == DIRECTORY,
                    pair.mode,
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut finder = Finder::default();
    let mut changes: Vec<(usize, ModeChange)> = Vec::new();
    for index in order {
        if ctx.checkpoint().is_err() {
            report.cancelled = true;
            break;
        }
        let pair = &pairs[index];
        // The errors say what happened without the path: whoever shows them
        // names the file (`op_toast`), and a path in both would say it twice.
        match set_one(&mut finder, anchor, pair) {
            Err(Missed::Gone) => report.gone += 1,
            Err(Missed::Failed(why)) => report.errors.push((pair.path.clone(), why)),
            Ok(Set::Link) => report.links += 1,
            Ok(Set::Unchanged) => report.unchanged += 1,
            Ok(Set::Changed(change)) => {
                report.changed += 1;
                if !report.unrecorded {
                    changes.push((index, change));
                    // Past the bound a record would be megabytes of paths
                    // held for a `u` nobody expects to cover a whole disk:
                    // the change stands, unrecorded, and the toast says so.
                    if changes.len() > cap {
                        report.unrecorded = true;
                        changes = Vec::new();
                    }
                }
            }
        }
        ctx.advance(0, 1);
    }
    // Kept in the order the paths were planned, parents first, which is the
    // order a person reading the record would expect.
    changes.sort_by_key(|(index, _)| *index);
    if !changes.is_empty() {
        report.record = Some(OpRecord::Mode {
            changes: changes.into_iter().map(|(_, change)| change).collect(),
        });
    }
    report
}

/// What happened to one planned path.
enum Set {
    Link,
    Unchanged,
    Changed(ModeChange),
}

/// Why one planned path was not set.
enum Missed {
    Gone,
    Failed(String),
}

/// Find one planned path again, check it is what the plan saw, and set it.
fn set_one(finder: &mut Finder, anchor: &Path, pair: &Planned) -> std::result::Result<Set, Missed> {
    let expected = pair.mode & FILE_TYPE;
    let (file, meta, depth) = finder
        .open(anchor, &pair.path, expected == DIRECTORY)
        .map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                Missed::Gone
            } else if expected == DIRECTORY && e.raw_os_error() == Some(libc::ENOTDIR) {
                Missed::Failed(NOT_THE_SAME_KIND.to_string())
            } else {
                Missed::Failed(lookup_failed(&e))
            }
        })?;
    let before = meta.mode();
    if expected == 0 && meta.file_type().is_symlink() {
        return Ok(Set::Link);
    }
    if (expected != 0 && before & FILE_TYPE != expected) || meta.file_type().is_symlink() {
        return Err(Missed::Failed(NOT_THE_SAME_KIND.to_string()));
    }
    let target = (before & !PERMISSIONS) | (pair.mode & PERMISSIONS);
    if target == before {
        return Ok(Set::Unchanged);
    }
    set_mode_of(&file, target).map_err(Missed::Failed)?;
    let after = file.metadata().map(|meta| meta.mode()).unwrap_or(target);
    Ok(Set::Changed(ModeChange {
        path: pair.path.clone(),
        before,
        after,
        depth,
        dev: meta.dev(),
        ino: meta.ino(),
    }))
}

/// The order to set `items` in, each `(path, is a folder, the mode it is
/// getting)`, as indices into it.
///
/// Folders that will be searchable are set first, shallowest first, so each
/// is open before anything under it is reached. Everything else follows,
/// deepest first, so a folder that is closing is closed only once what is
/// under it has been set. Depth is counted in path components, which puts a
/// parent before its children whatever order the items came in.
pub fn safe_order(items: &[(&Path, bool, u32)]) -> Vec<usize> {
    let depth = |index: &usize| items[*index].0.components().count();
    let opening = |index: &usize| items[*index].1 && items[*index].2 & OWNER_SEARCH != 0;
    let mut first: Vec<usize> = (0..items.len()).filter(opening).collect();
    first.sort_by_key(depth);
    let mut rest: Vec<usize> = (0..items.len()).filter(|i| !opening(i)).collect();
    rest.sort_by_key(|index| std::cmp::Reverse(depth(index)));
    first.extend(rest);
    first
}

/// Put every path's mode back, if every one of them is still the file the
/// change set, with the mode the change left.
///
/// Checked in full first: a change to forty files that can put back
/// thirty-nine should put back none, and say which one is in the way. The
/// one path that cannot be checked first is one inside a folder the change
/// itself shut to its owner: it is checked the moment that folder is open
/// again. Every path is checked again as it is restored, found the way the
/// change found it (from the folder it was asked in, following no link), so
/// a folder swapped for a link since cannot send a restore outside the tree.
/// If a check or a restore fails after some paths have gone back, the rest
/// are handed back as the remainder, so a second `u` finishes rather than
/// refusing over the half that is right.
pub(super) fn undo(changes: &[ModeChange], ctx: &TaskCtx) -> UndoAttempt {
    let refuse = |e: DfError| UndoAttempt {
        result: Err(e),
        remaining: None,
    };
    if changes.is_empty() {
        return refuse(DfError::Op("nothing to undo".to_string()));
    }
    if let Err(why) = proc_ready() {
        return refuse(DfError::Op(format!("cannot undo: {why}")));
    }
    // The folders this change left shut to their owner: what is under one
    // cannot be looked at until the undo has opened it.
    let shut: HashSet<&Path> = changes
        .iter()
        .filter(|change| change.is_dir() && change.after & OWNER_SEARCH == 0)
        .map(|change| change.path.as_path())
        .collect();
    let mut finder = Finder::default();
    for change in changes {
        match find(&mut finder, change) {
            Ok((_, meta)) => {
                if let Err(e) = still_as_left(change, &meta) {
                    return refuse(e);
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::PermissionDenied
                    && change.path.ancestors().skip(1).any(|up| shut.contains(up)) => {}
            Err(e) => return refuse(unreachable(change, &e)),
        }
    }

    let order = safe_order(
        &changes
            .iter()
            .map(|change| (change.path.as_path(), change.is_dir(), change.before))
            .collect::<Vec<_>>(),
    );
    let mut restored = vec![false; changes.len()];
    let remainder = |restored: &[bool]| {
        let rest: Vec<ModeChange> = changes
            .iter()
            .zip(restored)
            .filter(|(_, done)| !**done)
            .map(|(change, _)| change.clone())
            .collect();
        (rest.len() < changes.len()).then_some(OpRecord::Mode { changes: rest })
    };
    let mut finder = Finder::default();
    for index in order {
        let change = &changes[index];
        let step = find(&mut finder, change)
            .map_err(|e| unreachable(change, &e))
            .and_then(|(file, meta)| still_as_left(change, &meta).map(|()| file))
            .and_then(|file| ctx.checkpoint().map(|()| file))
            .and_then(|file| {
                set_mode_of(&file, change.before).map_err(|why| {
                    DfError::Op(format!("cannot undo: {}: {why}", change.path.display()))
                })
            });
        if let Err(e) = step {
            return UndoAttempt {
                result: Err(e),
                remaining: remainder(&restored),
            };
        }
        restored[index] = true;
        ctx.advance(0, 1);
    }
    UndoAttempt {
        result: Ok(UndoReport {
            description: format!(
                "Restored permissions of {}",
                plural(changes.len(), "item", "items")
            ),
            touched: changes.iter().map(|change| change.path.clone()).collect(),
        }),
        remaining: None,
    }
}

/// A recorded path, found again from the folder its change was asked in.
fn find(finder: &mut Finder, change: &ModeChange) -> io::Result<(File, std::fs::Metadata)> {
    let anchor = change.anchor()?;
    let (file, meta, _) = finder.open(&anchor, &change.path, change.is_dir())?;
    Ok((file, meta))
}

/// Whether a path is still the file the change set, with the `st_mode` it
/// left, and the sentence that says what moved if not.
fn still_as_left(change: &ModeChange, now: &std::fs::Metadata) -> Result<()> {
    if now.file_type().is_symlink() || now.mode() & FILE_TYPE != change.after & FILE_TYPE {
        return Err(DfError::Op(format!(
            "cannot undo: {} is not the same kind of file any more",
            change.path.display()
        )));
    }
    if (now.dev(), now.ino()) != (change.dev, change.ino) {
        return Err(DfError::Op(format!(
            "cannot undo: {} is not the file that was changed — it was replaced since",
            change.path.display()
        )));
    }
    if now.mode() != change.after {
        return Err(DfError::Op(format!(
            "cannot undo: the permissions of {} have changed since",
            change.path.display()
        )));
    }
    Ok(())
}

/// Why a recorded path cannot be found again: gone, swapped for a link, or
/// out of reach.
fn unreachable(change: &ModeChange, e: &io::Error) -> DfError {
    if e.kind() == io::ErrorKind::NotFound {
        DfError::Op(format!(
            "cannot undo: {} is no longer there",
            change.path.display()
        ))
    } else if swapped(e) {
        DfError::Op(format!(
            "cannot undo: {} is not where it was — a link or a file is in the way",
            change.path.display()
        ))
    } else {
        DfError::Op(format!("cannot undo: {}: {e}", change.path.display()))
    }
}

/// "1 item", "1,234 items".
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", grouped(n as u64))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;
    use crate::ops::journal::{undo_attempt, Journal};

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().mode() & MODE_BITS
    }

    fn set(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn one_mode_is_nine_plain_cells_and_its_octal() {
        use Cell::{Off, On};
        let grid = Grid::of_mode(0o754);
        assert_eq!(grid.octal(), "754");
        assert_eq!(grid.mode(), Some(0o754));
        assert!(!grid.is_mixed());
        assert_eq!(grid.cells, [On, On, On, On, Off, On, On, Off, Off]);
        // The file type and the special bits are not the grid's.
        assert_eq!(Grid::of_mode(0o104755), Grid::of_mode(0o755));
    }

    #[test]
    fn modes_that_disagree_are_mixed_where_they_do() {
        use Cell::{Mixed, Off, On};
        let grid = Grid::of([0o644, 0o600]);
        assert_eq!(
            grid.cells,
            [On, On, Off, Mixed, Off, Off, Mixed, Off, Off],
            "the owner agrees, the rest do not"
        );
        assert!(grid.is_mixed());
        assert_eq!(grid.mode(), None);
        assert_eq!(grid.octal(), "6––", "a triad with a mixed cell is a dash");
        assert_eq!(Grid::of([0o755, 0o715]).octal(), "7–5");
        assert_eq!(Grid::of(Vec::<u32>::new()).octal(), "000");
    }

    #[test]
    fn three_or_four_octal_digits_are_a_grid_and_nothing_else_is() {
        assert_eq!(Grid::parse("755"), Some(Grid::of_mode(0o755)));
        assert_eq!(Grid::parse("0644"), Some(Grid::of_mode(0o644)));
        assert_eq!(
            Grid::parse("4755"),
            Some(Grid::of_mode(0o755)),
            "the special digit is accepted and changes nothing"
        );
        for bad in ["", "7", "75", "75555", "758", "7a5", "-75"] {
            assert_eq!(Grid::parse(bad), None, "{bad:?}");
        }
        // Every mode the nine bits can make, there and back.
        for mode in 0..=0o777 {
            let grid = Grid::of_mode(mode);
            assert_eq!(Grid::parse(&grid.octal()), Some(grid), "{mode:o}");
        }
    }

    #[test]
    fn a_mixed_cell_toggles_on_and_then_off() {
        let mut grid = Grid::of([0o644, 0o600]);
        grid.toggle(3);
        assert_eq!(grid.cells[3], Cell::On);
        grid.toggle(3);
        assert_eq!(grid.cells[3], Cell::Off);
        grid.toggle(3);
        assert_eq!(grid.cells[3], Cell::On);
        // Past the ninth is nothing, not a panic.
        grid.toggle(99);
    }

    #[test]
    fn applying_keeps_mixed_bits_and_special_bits_per_item() {
        // Group read left mixed, everybody's read off, owner execute on.
        let mut grid = Grid::of([0o644, 0o600]);
        grid.toggle(2);
        grid.cells[6] = Cell::Off;
        assert_eq!(grid.apply(0o644), 0o740, "keeps its own group read");
        assert_eq!(grid.apply(0o600), 0o700, "keeps its own lack of one");
        // Setuid and the file type ride through untouched.
        assert_eq!(Grid::of_mode(0o700).apply(0o104755), 0o104700);
    }

    #[test]
    fn the_x_rule_gives_execute_wherever_read_is() {
        assert_eq!(searchable(0o644), 0o755);
        assert_eq!(searchable(0o600), 0o700);
        assert_eq!(searchable(0o640), 0o750);
        assert_eq!(searchable(0o200), 0o200, "write alone opens nothing");
        assert_eq!(searchable(0o2644), 0o2755);
    }

    /// One selected item, below `anchor`.
    fn item(path: &Path, mode: u32) -> Planned {
        Planned {
            path: path.to_path_buf(),
            mode,
        }
    }

    fn inode(path: &Path) -> (u64, u64) {
        let meta = std::fs::symlink_metadata(path).unwrap();
        (meta.dev(), meta.ino())
    }

    #[test]
    fn chmod_sets_each_mode_and_journals_before_and_after() {
        let t = TempTree::new("mode-apply");
        let a = t.file("a.txt", b"a");
        let b = t.file("b.txt", b"b");
        let same = t.file("same.txt", b"s");
        set(&a, 0o644);
        set(&b, 0o600);
        set(&same, 0o640);

        let report = chmod(
            t.path(),
            &[
                item(&a, 0o100755),
                item(&b, 0o100640),
                item(&same, 0o100640),
            ],
            &ctx(),
        );
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!((report.changed, report.unchanged), (2, 1));
        assert_eq!(mode_of(&a), 0o755);
        assert_eq!(mode_of(&b), 0o640);
        let Some(OpRecord::Mode { changes }) = &report.record else {
            panic!("no mode record: {:?}", report.record);
        };
        let (a_dev, a_ino) = inode(&a);
        let (b_dev, b_ino) = inode(&b);
        assert_eq!(
            changes,
            &vec![
                ModeChange {
                    path: a.clone(),
                    before: 0o100644,
                    after: 0o100755,
                    depth: 1,
                    dev: a_dev,
                    ino: a_ino,
                },
                ModeChange {
                    path: b.clone(),
                    before: 0o100600,
                    after: 0o100640,
                    depth: 1,
                    dev: b_dev,
                    ino: b_ino,
                },
            ],
            "an item already right is not in the record"
        );
        assert_eq!(
            report.record.as_ref().unwrap().describe(),
            "changed permissions of 2 items"
        );
        assert_eq!(report.message(), "Permissions set on 2 items");

        let mut journal = Journal::default();
        journal.record(report.record.unwrap());
        let undone = journal.undo(&ctx()).unwrap();
        assert_eq!(undone.description, "Restored permissions of 2 items");
        assert_eq!(mode_of(&a), 0o644);
        assert_eq!(mode_of(&b), 0o600);
        assert_eq!(mode_of(&same), 0o640);
        assert!(journal.is_empty());
    }

    #[test]
    fn the_special_bits_are_kept_whatever_the_pair_says() {
        let t = TempTree::new("mode-special");
        let dir = t.dir("shared");
        set(&dir, 0o1777);
        let report = chmod(t.path(), &[item(&dir, 0o755)], &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(mode_of(&dir), 0o1755, "the sticky bit went");
    }

    #[test]
    fn undo_refuses_when_a_mode_changed_underneath_and_restores_nothing() {
        let t = TempTree::new("mode-refuse");
        let a = t.file("a.txt", b"a");
        let b = t.file("b.txt", b"b");
        set(&a, 0o644);
        set(&b, 0o644);
        let report = chmod(t.path(), &[item(&a, 0o600), item(&b, 0o600)], &ctx());
        let record = report.record.unwrap();

        // Somebody else changes one of them.
        set(&b, 0o640);
        let attempt = undo_attempt(&record, &ctx());
        let error = attempt.result.unwrap_err().to_string();
        assert!(error.contains("b.txt"), "{error}");
        assert!(error.contains("changed since"), "{error}");
        assert!(attempt.remaining.is_none());
        assert_eq!(mode_of(&a), 0o600, "a was put back though b was in the way");

        // …and one that is gone is named as gone.
        set(&b, 0o600);
        std::fs::remove_file(&a).unwrap();
        let error = undo_attempt(&record, &ctx())
            .result
            .unwrap_err()
            .to_string();
        assert!(error.contains("no longer there"), "{error}");
        assert_eq!(mode_of(&b), 0o600);
    }

    /// A file deleted and made again with the mode the change left is not
    /// the file the change set: the undo refuses rather than give it a mode
    /// meant for the one before it.
    #[test]
    fn undo_refuses_a_file_replaced_by_another_with_the_same_mode() {
        let t = TempTree::new("mode-identity");
        let a = t.file("a.txt", b"a");
        set(&a, 0o644);
        let record = chmod(t.path(), &[item(&a, 0o600)], &ctx()).record.unwrap();
        // Made while the first still exists, so it cannot be given its inode
        // number, and moved over it.
        let other = t.file("a.new", b"another");
        set(&other, 0o600);
        std::fs::rename(&other, &a).unwrap();
        let error = undo_attempt(&record, &ctx())
            .result
            .unwrap_err()
            .to_string();
        assert!(error.contains("replaced"), "{error}");
        assert_eq!(mode_of(&a), 0o600, "the new file was given the old mode");
    }

    #[test]
    fn a_recursive_plan_gives_folders_the_x_rule_and_skips_links() {
        let t = TempTree::new("mode-plan");
        let top = t.dir("top");
        let file = t.file("top/notes.txt", b"n");
        let inner = t.dir("top/inner");
        let deep = t.file("top/inner/deep.txt", b"d");
        let outside = t.file("outside.txt", b"o");
        let link = t.symlink(&outside, "top/link");
        let dir_link = t.symlink(&inner, "top/inner-link");
        set(&outside, 0o600);
        for dir in [&top, &inner] {
            set(dir, 0o700);
        }
        for f in [&file, &deep] {
            set(f, 0o600);
        }

        let grid = Grid::of_mode(0o644);
        let planned = plan(t.path(), std::slice::from_ref(&top), &grid, true, &ctx()).unwrap();
        assert!(planned.errors.is_empty(), "{:?}", planned.errors);
        assert_eq!(planned.links, 2);
        let mut pairs = planned.pairs.clone();
        pairs.sort_by(|a, b| a.path.cmp(&b.path));
        let mut expected = vec![
            item(&top, 0o40755),
            item(&inner, 0o40755),
            item(&file, 0o100644),
            item(&deep, 0o100644),
        ];
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(pairs, expected);
        assert!(!pairs.iter().any(|p| p.path == link || p.path == dir_link));

        let report = chmod(t.path(), &planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.changed, 4);
        assert_eq!(mode_of(&top), 0o755);
        assert_eq!(mode_of(&inner), 0o755);
        assert_eq!(mode_of(&file), 0o644);
        assert_eq!(mode_of(&deep), 0o644);
        assert_eq!(mode_of(&outside), 0o600, "a link was followed");
        let Some(OpRecord::Mode { changes }) = &report.record else {
            panic!("no record");
        };
        let depth = |path: &Path| changes.iter().find(|c| c.path == path).map(|c| c.depth);
        assert_eq!(
            [depth(&top), depth(&inner), depth(&deep)],
            [Some(1), Some(2), Some(3)],
            "each recorded as far below the folder as it is"
        );

        // Not recursive: the folder gets the grid exactly, and nothing in it
        // is touched.
        let flat = plan(
            t.path(),
            std::slice::from_ref(&top),
            &Grid::of_mode(0o700),
            false,
            &ctx(),
        )
        .unwrap();
        assert_eq!(flat.pairs, vec![item(&top, 0o40700)]);
        // A link in the selection itself is skipped too.
        let linked = plan(t.path(), &[link], &grid, false, &ctx()).unwrap();
        assert!(linked.pairs.is_empty());
        assert_eq!(linked.links, 1);
    }

    #[test]
    fn folders_opening_go_first_and_folders_closing_go_last() {
        let items = [
            (Path::new("/t/a/b/g"), false, 0o600),
            (Path::new("/t/a/b"), true, 0o600),
            (Path::new("/t/a/f"), false, 0o600),
            (Path::new("/t/a"), true, 0o700),
        ];
        let order = safe_order(&items);
        let at = |i: usize| order.iter().position(|&o| o == i).unwrap();
        // `a` stays searchable, so it is set before anything under it.
        assert_eq!(order[0], 3, "{order:?}");
        // `b` is closing, so what is in it is set before it is.
        assert!(at(0) < at(1), "{order:?}");
        assert_eq!(order.len(), 4);
    }

    #[test]
    fn a_folder_closed_to_its_owner_is_closed_after_what_is_in_it() {
        let t = TempTree::new("mode-order");
        let top = t.dir("top");
        let file = t.file("top/f.txt", b"f");
        set(&top, 0o755);
        set(&file, 0o644);
        // Nothing for anybody: the folder is shut, and the X rule has no
        // read to give execute to. Setting the folder first would leave the
        // file out of reach.
        let planned = plan(
            t.path(),
            std::slice::from_ref(&top),
            &Grid::of_mode(0),
            true,
            &ctx(),
        )
        .unwrap();
        let report = chmod(t.path(), &planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.changed, 2);
        assert_eq!(mode_of(&top), 0);
        // …and the undo opens it again before it reaches inside.
        let attempt = undo_attempt(&report.record.unwrap(), &ctx());
        attempt.result.unwrap();
        assert_eq!(mode_of(&top), 0o755);
        assert_eq!(mode_of(&file), 0o644);
    }

    #[test]
    fn a_cancelled_change_records_only_what_it_did() {
        let t = TempTree::new("mode-cancel");
        let a = t.file("a.txt", b"a");
        set(&a, 0o644);
        let flags = std::sync::Arc::new(crate::tasks::TaskFlags::new());
        flags.cancel();
        let cancelled = TaskCtx::with_sink(flags, std::sync::Arc::new(crate::tasks::NullSink));
        let report = chmod(t.path(), &[item(&a, 0o600)], &cancelled);
        assert!(report.cancelled);
        assert!(report.record.is_none(), "nothing happened, nothing to undo");
        assert_eq!(mode_of(&a), 0o644);
        assert!(matches!(
            plan(t.path(), &[a], &Grid::of_mode(0), false, &cancelled),
            Err(DfError::Cancelled)
        ));
    }

    /// Swap `path` (a folder) for a link to `target` behind the change's back.
    fn swap_for_link(path: &Path, target: &Path) {
        let aside = path.with_extension("moved");
        std::fs::rename(path, &aside).unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
    }

    /// **The swap this module is built against.** A folder in the tree is
    /// planned, then swapped for a link to a file outside it before the
    /// change runs: the change must not follow the link to the file, nor go
    /// down it to reach what was under the folder, and it says so.
    #[test]
    fn a_folder_swapped_for_a_link_after_the_plan_sends_nothing_outside() {
        let t = TempTree::new("mode-swap");
        let anchor = t.dir("tree");
        let top = t.dir("tree/top");
        let sub = t.dir("tree/top/sub");
        let inner = t.file("tree/top/sub/inner.txt", b"i");
        let kept = t.file("tree/top/kept.txt", b"k");
        let outside = t.file("outside.txt", b"o");
        let far = t.dir("far");
        let far_inner = t.file("far/inner.txt", b"f");
        for path in [&inner, &kept, &outside, &far_inner] {
            set(path, 0o644);
        }
        for dir in [&top, &sub, &far] {
            set(dir, 0o755);
        }
        let planned = plan(
            &anchor,
            std::slice::from_ref(&top),
            &Grid::of_mode(0o600),
            true,
            &ctx(),
        )
        .unwrap();
        assert_eq!(planned.pairs.len(), 4);

        // A link to a file where the folder was.
        swap_for_link(&sub, &outside);
        let report = chmod(&anchor, &planned.pairs, &ctx());
        assert_eq!(mode_of(&outside), 0o644, "the change followed the link");
        let failed: Vec<&PathBuf> = report.errors.iter().map(|(path, _)| path).collect();
        assert!(failed.contains(&&sub), "{:?}", report.errors);
        assert!(failed.contains(&&inner), "{:?}", report.errors);
        assert_eq!(mode_of(&kept), 0o600, "the rest of the tree still changed");
        assert_eq!(mode_of(&top), 0o700);

        // A link to a folder outside with a file of the same name in it:
        // going down the link would find `inner.txt` and set it.
        std::fs::remove_file(&sub).unwrap();
        std::os::unix::fs::symlink(&far, &sub).unwrap();
        let report = chmod(&anchor, &planned.pairs, &ctx());
        assert_eq!(mode_of(&far_inner), 0o644, "the change went down the link");
        assert_eq!(mode_of(&far), 0o755, "the change set the link's target");
        let failed: Vec<&PathBuf> = report.errors.iter().map(|(path, _)| path).collect();
        assert!(
            failed.contains(&&sub) && failed.contains(&&inner),
            "{:?}",
            report.errors
        );
        // What was under the folder, moved aside, is not the change's either.
        assert_eq!(
            mode_of(&sub.with_extension("moved").join("inner.txt")),
            0o644
        );
    }

    /// A row two folders below the one on screen — a search hit — is found
    /// from the folder on screen, both folders on the way looked up and
    /// neither followed: the middle one swapped for a link to a copy of the
    /// same tree outside sends the change nowhere.
    #[test]
    fn a_row_two_folders_down_is_found_without_following_either() {
        let t = TempTree::new("mode-deep-row");
        let anchor = t.dir("project");
        let hit = t.file("project/src/app/foo.rs", b"f");
        let decoy = t.file("elsewhere/app/foo.rs", b"d");
        set(&hit, 0o644);
        set(&decoy, 0o644);
        let planned = plan(
            &anchor,
            std::slice::from_ref(&hit),
            &Grid::of_mode(0o600),
            false,
            &ctx(),
        )
        .unwrap();
        assert_eq!(planned.pairs, vec![item(&hit, 0o100600)]);

        // Straight through, it is set and recorded three components down.
        let report = chmod(&anchor, &planned.pairs, &ctx());
        assert_eq!(mode_of(&hit), 0o600);
        let record = report.record.unwrap();
        let OpRecord::Mode { changes } = &record else {
            panic!("not a mode record");
        };
        assert_eq!(changes[0].depth, 3);
        undo_attempt(&record, &ctx()).result.unwrap();
        assert_eq!(mode_of(&hit), 0o644);

        // The middle folder swapped for a link to the decoy's tree.
        swap_for_link(&t.join("project/src"), &t.join("elsewhere"));
        let report = chmod(&anchor, &planned.pairs, &ctx());
        assert_eq!(mode_of(&decoy), 0o644, "the change followed src");
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(
            report.errors[0].1.contains("not where it was"),
            "{:?}",
            report.errors
        );
    }

    /// A target that is not below the folder the change is asked in is
    /// refused, out loud, and not looked up some other way.
    #[test]
    fn a_target_outside_the_folder_is_refused() {
        let t = TempTree::new("mode-outside");
        let anchor = t.dir("here");
        let there = t.file("there/a.txt", b"a");
        let sibling = t.file("here-and-more/b.txt", b"b");
        set(&there, 0o644);
        set(&sibling, 0o644);
        let grid = Grid::of_mode(0o600);
        let planned = plan(
            &anchor,
            &[there.clone(), sibling.clone(), anchor.clone()],
            &grid,
            false,
            &ctx(),
        )
        .unwrap();
        assert!(planned.pairs.is_empty());
        assert_eq!(planned.errors.len(), 3, "{:?}", planned.errors);
        assert!(planned
            .errors
            .iter()
            .all(|(_, why)| why.contains("not inside the folder")));
        // …and a pair handed straight to the change, the same.
        let report = chmod(&anchor, &[item(&there, 0o100600)], &ctx());
        assert_eq!(report.errors.len(), 1);
        assert_eq!(mode_of(&there), 0o644);
    }

    /// The finder's cache is keyed by the anchor as well as the folder: the
    /// same folder reached as an anchor (followed) and from an anchor above
    /// it (not followed) are two lookups, and the second must not borrow the
    /// first's descriptor.
    #[test]
    fn a_folder_found_as_an_anchor_is_not_lent_to_a_deeper_lookup() {
        let t = TempTree::new("mode-cache");
        let root = t.dir("root");
        let far = t.dir("far");
        t.file("far/a.txt", b"a");
        t.file("far/b.txt", b"b");
        let sub = t.symlink(&far, "root/sub");
        let mut finder = Finder::default();
        // As an anchor, `sub` is where the person is, and it is followed.
        assert!(finder.open(&sub, &sub.join("a.txt"), false).is_ok());
        // From `root`, `sub` is a component below the anchor, and a link.
        let deeper = finder.open(&root, &sub.join("b.txt"), false);
        assert!(
            deeper.as_ref().is_err_and(swapped),
            "{:?}",
            deeper.map(|_| ())
        );
    }

    /// The same swap at the top: an item selected, planned, and replaced by
    /// a link before the change runs.
    #[test]
    fn a_selected_file_swapped_for_a_link_is_left_alone() {
        let t = TempTree::new("mode-swap-top");
        let a = t.file("a.txt", b"a");
        let outside = t.file("elsewhere/secret.txt", b"s");
        set(&a, 0o644);
        set(&outside, 0o644);
        let planned = plan(
            t.path(),
            std::slice::from_ref(&a),
            &Grid::of_mode(0o666),
            false,
            &ctx(),
        )
        .unwrap();
        std::fs::remove_file(&a).unwrap();
        std::os::unix::fs::symlink(&outside, &a).unwrap();
        let report = chmod(t.path(), &planned.pairs, &ctx());
        assert_eq!(mode_of(&outside), 0o644);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(
            report.errors[0].1.contains("not the same kind"),
            "{:?}",
            report.errors
        );
        assert!(report.record.is_none());
    }

    /// The undo finds its paths the same way: a folder swapped for a link to
    /// one outside, whose file has exactly the mode the change left, is a
    /// refusal, and nothing outside is restored.
    #[test]
    fn an_undo_does_not_follow_a_folder_swapped_for_a_link() {
        let t = TempTree::new("mode-swap-undo");
        let anchor = t.dir("tree");
        let top = t.dir("tree/top");
        let sub = t.dir("tree/top/sub");
        let inner = t.file("tree/top/sub/inner.txt", b"i");
        let far = t.dir("far");
        let far_inner = t.file("far/inner.txt", b"f");
        set(&inner, 0o644);
        set(&top, 0o755);
        set(&sub, 0o755);
        let planned = plan(
            &anchor,
            std::slice::from_ref(&top),
            &Grid::of_mode(0o600),
            true,
            &ctx(),
        )
        .unwrap();
        let record = chmod(&anchor, &planned.pairs, &ctx()).record.unwrap();
        // Outside, a file with the mode the change left, where the link leads.
        set(&far_inner, 0o600);
        set(&far, 0o700);
        swap_for_link(&sub, &far);
        let attempt = undo_attempt(&record, &ctx());
        let error = attempt.result.unwrap_err().to_string();
        assert!(error.contains("not where it was"), "{error}");
        assert_eq!(mode_of(&far_inner), 0o600, "the undo went down the link");
        assert_eq!(mode_of(&far), 0o700);
        assert_eq!(mode_of(&top), 0o700, "restored anything before refusing");
    }

    /// The folder on screen is where the person is, and a link on the way
    /// to it is theirs to follow: only what is below it is looked up
    /// without following.
    #[test]
    fn the_folder_the_change_is_asked_in_may_be_reached_through_a_link() {
        let t = TempTree::new("mode-anchor");
        let real = t.file("real/a.txt", b"a");
        set(&real, 0o644);
        let shortcut = t.symlink(t.join("real"), "shortcut");
        let through = shortcut.join("a.txt");
        let planned = plan(&shortcut, &[through], &Grid::of_mode(0o600), false, &ctx()).unwrap();
        let report = chmod(&shortcut, &planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(mode_of(&real), 0o600);
        undo_attempt(&report.record.unwrap(), &ctx())
            .result
            .unwrap();
        assert_eq!(mode_of(&real), 0o644);
    }

    /// A file nobody can read is a file whose mode can still be put right:
    /// the descriptor it is set through is opened only to name it.
    #[test]
    fn a_file_with_no_permissions_at_all_can_be_given_some() {
        let t = TempTree::new("mode-zero");
        let a = t.file("locked.txt", b"l");
        set(&a, 0);
        let planned = plan(
            t.path(),
            std::slice::from_ref(&a),
            &Grid::of_mode(0o644),
            false,
            &ctx(),
        )
        .unwrap();
        let report = chmod(t.path(), &planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(mode_of(&a), 0o644);
    }

    /// A folder of mode 000 with "everything inside": this change opens it,
    /// but could not list it to plan, so the toast says a second Apply
    /// reaches inside — which it does.
    #[test]
    fn a_shut_folder_asks_for_a_second_apply_and_the_second_goes_inside() {
        let t = TempTree::new("mode-again");
        let locked = t.dir("locked");
        let inside = t.file("locked/a.txt", b"a");
        set(&inside, 0o600);
        set(&locked, 0);
        if std::fs::read_dir(&locked).is_ok() {
            // Root reads a folder of mode 000; there is nothing to test.
            eprintln!("skipped: this user can list a folder of mode 000");
            set(&locked, 0o755);
            return;
        }
        let grid = Grid::of_mode(0o644);
        let first = plan(t.path(), std::slice::from_ref(&locked), &grid, true, &ctx()).unwrap();
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        assert_eq!(first.again, vec![locked.clone()]);
        let mut report = chmod(t.path(), &first.pairs, &ctx());
        report.absorb(first);
        assert_eq!(
            report.message(),
            "Permissions set on 1 item · Apply again to reach inside locked"
        );
        assert_eq!(mode_of(&locked), 0o755);
        assert_eq!(mode_of(&inside), 0o600, "untouched until the second Apply");

        let second = plan(t.path(), std::slice::from_ref(&locked), &grid, true, &ctx()).unwrap();
        assert!(second.again.is_empty());
        let report = chmod(t.path(), &second.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(mode_of(&inside), 0o644);
    }

    /// A planned path that is gone when its turn comes is counted, once, not
    /// reported as an error per file — at the plan or at the change.
    #[test]
    fn paths_that_vanished_are_counted_as_gone() {
        let t = TempTree::new("mode-gone");
        let a = t.file("a.txt", b"a");
        let b = t.file("b.txt", b"b");
        let c = t.file("c.txt", b"c");
        for f in [&a, &b, &c] {
            set(f, 0o644);
        }
        let grid = Grid::of_mode(0o600);
        std::fs::remove_file(&c).unwrap();
        let planned = plan(
            t.path(),
            &[a.clone(), b.clone(), c.clone()],
            &grid,
            false,
            &ctx(),
        )
        .unwrap();
        assert_eq!((planned.pairs.len(), planned.gone), (2, 1));
        std::fs::remove_file(&b).unwrap();
        let mut report = chmod(t.path(), &planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!((report.changed, report.gone), (1, 1));
        report.absorb(planned);
        assert_eq!(
            report.message(),
            "Permissions set on 1 item · 2 no longer there"
        );
    }

    /// A file that appears after the plan is not in it, and the change
    /// leaves it as it was made.
    #[test]
    fn a_file_that_appears_after_the_plan_is_left_alone() {
        let t = TempTree::new("mode-late");
        let dir = t.dir("dir");
        let early = t.file("dir/early.txt", b"e");
        set(&early, 0o644);
        let planned = plan(
            t.path(),
            std::slice::from_ref(&dir),
            &Grid::of_mode(0o600),
            true,
            &ctx(),
        )
        .unwrap();
        let late = t.file("dir/late.txt", b"l");
        set(&late, 0o644);
        let report = chmod(t.path(), &planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(mode_of(&early), 0o600);
        assert_eq!(mode_of(&late), 0o644, "a file nobody planned was changed");
        let Some(OpRecord::Mode { changes }) = &report.record else {
            panic!("no record");
        };
        assert!(!changes.iter().any(|change| change.path == late));
    }

    /// Past the bound, the change is made and left unrecorded, and the toast
    /// says it cannot be undone.
    #[test]
    fn a_change_past_the_bound_is_made_and_says_it_cannot_be_undone() {
        let t = TempTree::new("mode-cap");
        let files: Vec<PathBuf> = (0..3)
            .map(|i| {
                let f = t.file(format!("f{i}.txt"), b"x");
                set(&f, 0o644);
                f
            })
            .collect();
        let pairs: Vec<Planned> = files.iter().map(|f| item(f, 0o100600)).collect();
        let report = chmod_capped(t.path(), &pairs, &ctx(), 2);
        assert!(report.unrecorded);
        assert!(report.record.is_none());
        assert_eq!(report.changed, 3);
        assert!(files.iter().all(|f| mode_of(f) == 0o600));
        assert_eq!(
            report.message(),
            "Permissions set on 3 items · too many to undo"
        );
        // The real bound is the copy manifest's.
        let within = chmod(t.path(), &pairs, &ctx());
        assert!(within.record.is_none() && within.unchanged == 3);
    }

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
