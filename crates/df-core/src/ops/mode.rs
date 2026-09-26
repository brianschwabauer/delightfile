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
//! folder the change was asked in is reached by its path: each component is
//! opened from the one above it without following it, and the mode is set on
//! what was opened (see "Finding a path without following a link" below).
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
//! a file whose group the caller is not in. The inverse checks that every
//! path still has exactly the mode the change left before it restores any of
//! them, the journal's refusal-before-touching rule, and a forward re-apply is
//! the same walk with `before` and `after` swapped.

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
/// file type and all, and how to find the path again.
///
/// The whole of it rather than the twelve bits `chmod` sets, for two reasons.
/// Comparing it notices a file that was replaced by a folder of the same name
/// as well as one whose bits moved. And it says which paths are folders
/// without asking the disk, which the undo needs for a folder the change
/// shut: nothing under it can be asked anything until it is open again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChange {
    pub path: PathBuf,
    pub before: u32,
    pub after: u32,
    /// How many of `path`'s last components are below the folder the change
    /// was asked in: 1 for an item that was selected, one more for each
    /// folder down inside one. The undo looks each of them up from that
    /// folder without following a link, as the change did ([`Planned::depth`]).
    pub depth: usize,
}

impl ModeChange {
    /// Whether the path is a folder, by the file type the record kept.
    fn is_dir(&self) -> bool {
        self.after & FILE_TYPE == DIRECTORY
    }
}

/// `st_mode`'s file-type field, and its value for a folder.
const FILE_TYPE: u32 = 0o170000;
const DIRECTORY: u32 = 0o040000;

/// One path a change will set, and how to find it again when it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub path: PathBuf,
    /// The mode to set, with the file type the plan found at the path: the
    /// kind of file it must still be when it is set. With no file type in it,
    /// any kind will do but a link.
    pub mode: u32,
    /// How many of `path`'s last components are below the folder the change
    /// was asked in — the folder on screen, as the person reached it, links
    /// and all. 1 is an item that was selected in it; each folder down inside
    /// one adds 1. Those components are looked up one at a time from that
    /// folder, and none of them is followed if it is a link.
    pub depth: usize,
}

impl Planned {
    /// An item that was selected, by its path in the folder on screen.
    pub fn item(path: impl Into<PathBuf>, mode: u32) -> Planned {
        Planned {
            path: path.into(),
            mode,
            depth: 1,
        }
    }
}

/// What a change is about to do, worked out before any of it is done.
#[derive(Debug, Default)]
pub struct ModePlan {
    /// What [`chmod`] sets: each mode the grid applied to that path's own,
    /// parents before what is in them.
    pub pairs: Vec<Planned>,
    /// Links met and left alone.
    pub links: usize,
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
// and the undo already close. So nothing below the folder the change was
// asked in is reached by its path. Each component is opened from the folder
// above it, open already, with `O_PATH | O_NOFOLLOW` (and `O_DIRECTORY` for
// a folder), `fstat` says what was opened, and the mode is set through
// `/proc/self/fd/<n>`, a name that leads to that inode and no other.
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

/// Finds a planned or recorded path again: the folder the change was asked
/// in by its path, links followed, since that is where the person is; then
/// each component below it from the one above, none followed.
///
/// Keeps the last folder it opened. Paths come a folder's worth at a time,
/// so most lookups are one `open` and not one per component.
#[derive(Default)]
struct Finder {
    parent: Option<(PathBuf, Rc<File>)>,
}

impl Finder {
    /// The folder `path` is in, open, reached as [`Planned::depth`] says.
    fn parent(&mut self, path: &Path, depth: usize) -> io::Result<Rc<File>> {
        let components: Vec<Component<'_>> = path.components().collect();
        if depth == 0 || depth > components.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a path this change can find",
            ));
        }
        let parent: PathBuf = components[..components.len() - 1].iter().collect();
        if let Some((cached, dir)) = &self.parent {
            if *cached == parent {
                return Ok(Rc::clone(dir));
            }
        }
        let split = components.len() - depth;
        let anchor: PathBuf = components[..split].iter().collect();
        let anchor = if anchor.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            anchor
        };
        let mut dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY)
            .open(&anchor)?;
        for component in &components[split..components.len() - 1] {
            let Component::Normal(name) = component else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "not a path this change can find",
                ));
            };
            dir = open_in(&dir, name, true)?;
            if !dir.metadata()?.is_dir() {
                return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
            }
        }
        let dir = Rc::new(dir);
        self.parent = Some((parent, Rc::clone(&dir)));
        Ok(dir)
    }

    /// `path` itself, opened to name it and not followed, and what it is.
    fn open(
        &mut self,
        path: &Path,
        depth: usize,
        want_dir: bool,
    ) -> io::Result<(File, std::fs::Metadata)> {
        let name = file_name_of(path)?;
        let dir = self.parent(path, depth)?;
        let file = open_in(&dir, name, want_dir)?;
        let meta = file.metadata()?;
        Ok((file, meta))
    }
}

fn file_name_of(path: &Path) -> io::Result<&OsStr> {
    match path.components().next_back() {
        Some(Component::Normal(name)) => Ok(name),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a path this change can find",
        )),
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
/// ([`searchable`] for every folder). Links are counted and skipped.
///
/// Each target is looked up in the folder it is in without following it, and
/// the walk goes down by descriptors (see "Finding a path without following
/// a link" above): a folder renamed or swapped for a link after it was seen
/// cannot send the walk anywhere else. It walks with an explicit stack, so a
/// deep tree is a long `Vec` and not a stack overflow, and each pending
/// folder holds only the folder it is in open, so what is open at once is the
/// folders on the way down rather than every folder met. It asks `ctx`
/// between entries: the only error is [`DfError::Cancelled`], and everything
/// it cannot read is in [`ModePlan::errors`].
pub fn plan(targets: &[PathBuf], grid: &Grid, recursive: bool, ctx: &TaskCtx) -> Result<ModePlan> {
    let mut plan = ModePlan::default();
    if let Err(why) = proc_ready() {
        plan.errors = targets.iter().map(|t| (t.clone(), why.clone())).collect();
        return Ok(plan);
    }
    // Folders still to be listed: the folder each is in, open, and its path
    // for the record — never the path to open.
    let mut folders: Vec<(Rc<File>, PathBuf, usize)> = Vec::new();
    let mut finder = Finder::default();
    for target in targets {
        ctx.checkpoint()?;
        let looked = file_name_of(target).and_then(|name| {
            let dir = finder.parent(target, 1)?;
            let meta = stat_in(&dir, name)?;
            Ok((dir, meta))
        });
        match looked {
            Err(e) => plan.errors.push((target.clone(), lookup_failed(&e))),
            Ok((_, meta)) if meta.file_type().is_symlink() => plan.links += 1,
            Ok((dir, meta)) if recursive && meta.is_dir() => {
                plan.pairs.push(Planned::item(
                    target.clone(),
                    searchable(grid.apply(meta.mode())),
                ));
                folders.push((dir, target.clone(), 1));
            }
            Ok((_, meta)) => plan
                .pairs
                .push(Planned::item(target.clone(), grid.apply(meta.mode()))),
        }
    }
    while let Some((parent, folder, depth)) = folders.pop() {
        let opened = file_name_of(&folder).and_then(|name| open_in(&parent, name, true));
        drop(parent);
        let dir = match opened {
            Ok(dir) => Rc::new(dir),
            Err(e) => {
                plan.errors.push((folder, lookup_failed(&e)));
                continue;
            }
        };
        let listing = match std::fs::read_dir(named(&dir)) {
            Ok(listing) => listing,
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
                Err(e) => plan.errors.push((path, e.to_string())),
                Ok(meta) if meta.file_type().is_symlink() => plan.links += 1,
                Ok(meta) if meta.is_dir() => {
                    plan.pairs.push(Planned {
                        path: path.clone(),
                        mode: searchable(grid.apply(meta.mode())),
                        depth: depth + 1,
                    });
                    folders.push((Rc::clone(&dir), path, depth + 1));
                }
                Ok(meta) => plan.pairs.push(Planned {
                    path,
                    mode: grid.apply(meta.mode()),
                    depth: depth + 1,
                }),
            }
        }
    }
    Ok(plan)
}

/// What [`chmod`] did.
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
    /// More changed than a record holds: done, and not undoable.
    pub unrecorded: bool,
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

/// Set each path's mode, in the order that keeps every folder searchable
/// until what is under it has been set ([`safe_order`]).
///
/// Each path is found again as the plan found it — from the folder the change
/// was asked in, one component at a time, following no link — opened, and
/// `fstat`ed: a path that is no longer the kind of file the plan recorded,
/// or that has a link or a file where a folder on the way was, is left alone
/// and reported. The nine bits come from the pair; the special bits are kept
/// as the inode has them now, whatever the pair says. A link is skipped, and
/// a path that already has its mode is left untouched and not recorded.
/// Failures are collected rather than returned, and a cancel stops between
/// two paths with everything set so far recorded, so a `u` takes back
/// exactly the part that happened.
pub fn chmod(pairs: &[Planned], ctx: &TaskCtx) -> ModeReport {
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
        match set_one(&mut finder, pair) {
            Err(why) => report.errors.push((pair.path.clone(), why)),
            Ok(Set::Link) => report.links += 1,
            Ok(Set::Unchanged) => report.unchanged += 1,
            Ok(Set::Changed { before, after }) => {
                report.changed += 1;
                if !report.unrecorded {
                    changes.push((
                        index,
                        ModeChange {
                            path: pair.path.clone(),
                            before,
                            after,
                            depth: pair.depth,
                        },
                    ));
                    // Past the bound a record would be megabytes of paths
                    // held for a `u` nobody expects to cover a whole disk:
                    // the change stands, unrecorded, and the toast says so.
                    if changes.len() > MAX_MANIFEST_ENTRIES {
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
    Changed { before: u32, after: u32 },
}

/// Find one planned path again, check it is what the plan saw, and set it.
fn set_one(finder: &mut Finder, pair: &Planned) -> std::result::Result<Set, String> {
    let expected = pair.mode & FILE_TYPE;
    let (file, meta) = finder
        .open(&pair.path, pair.depth, expected == DIRECTORY)
        .map_err(|e| {
            if expected == DIRECTORY && e.raw_os_error() == Some(libc::ENOTDIR) {
                NOT_THE_SAME_KIND.to_string()
            } else {
                lookup_failed(&e)
            }
        })?;
    let before = meta.mode();
    if expected == 0 && meta.file_type().is_symlink() {
        return Ok(Set::Link);
    }
    if (expected != 0 && before & FILE_TYPE != expected) || meta.file_type().is_symlink() {
        return Err(NOT_THE_SAME_KIND.to_string());
    }
    let target = (before & !PERMISSIONS) | (pair.mode & PERMISSIONS);
    if target == before {
        return Ok(Set::Unchanged);
    }
    set_mode_of(&file, target)?;
    let after = file.metadata().map(|meta| meta.mode()).unwrap_or(target);
    Ok(Set::Changed { before, after })
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

/// Put every path's mode back, if every one of them still has the mode the
/// change left.
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
        match finder.open(&change.path, change.depth, change.is_dir()) {
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
        let step = finder
            .open(&change.path, change.depth, change.is_dir())
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

/// Whether a path's `st_mode` is still the one the change left, and the
/// sentence that says what moved if not.
fn still_as_left(change: &ModeChange, now: &std::fs::Metadata) -> Result<()> {
    if now.file_type().is_symlink() || now.mode() & FILE_TYPE != change.after & FILE_TYPE {
        return Err(DfError::Op(format!(
            "cannot undo: {} is not the same kind of file any more",
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
            &[
                Planned::item(a.clone(), 0o100755),
                Planned::item(b.clone(), 0o100640),
                Planned::item(same.clone(), 0o100640),
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
        assert_eq!(
            changes,
            &vec![
                ModeChange {
                    path: a.clone(),
                    before: 0o100644,
                    after: 0o100755,
                    depth: 1,
                },
                ModeChange {
                    path: b.clone(),
                    before: 0o100600,
                    after: 0o100640,
                    depth: 1,
                },
            ],
            "an item already right is not in the record"
        );
        assert_eq!(
            report.record.as_ref().unwrap().describe(),
            "changed permissions of 2 items"
        );

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
        let report = chmod(&[Planned::item(dir.clone(), 0o755)], &ctx());
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
        let report = chmod(
            &[
                Planned::item(a.clone(), 0o600),
                Planned::item(b.clone(), 0o600),
            ],
            &ctx(),
        );
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
        let planned = plan(std::slice::from_ref(&top), &grid, true, &ctx()).unwrap();
        assert!(planned.errors.is_empty(), "{:?}", planned.errors);
        assert_eq!(planned.links, 2);
        let mut pairs: Vec<(PathBuf, u32, usize)> = planned
            .pairs
            .iter()
            .map(|p| (p.path.clone(), p.mode, p.depth))
            .collect();
        pairs.sort();
        let mut expected = vec![
            (top.clone(), 0o40755, 1),
            (inner.clone(), 0o40755, 2),
            (file.clone(), 0o100644, 2),
            (deep.clone(), 0o100644, 3),
        ];
        expected.sort();
        assert_eq!(pairs, expected, "path, mode and how far below the folder");
        assert!(!pairs.iter().any(|(p, ..)| p == &link || p == &dir_link));

        let report = chmod(&planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.changed, 4);
        assert_eq!(mode_of(&top), 0o755);
        assert_eq!(mode_of(&inner), 0o755);
        assert_eq!(mode_of(&file), 0o644);
        assert_eq!(mode_of(&deep), 0o644);
        assert_eq!(mode_of(&outside), 0o600, "a link was followed");

        // Not recursive: the folder gets the grid exactly, and nothing in it
        // is touched.
        let flat = plan(
            std::slice::from_ref(&top),
            &Grid::of_mode(0o700),
            false,
            &ctx(),
        )
        .unwrap();
        assert_eq!(flat.pairs, vec![Planned::item(top.clone(), 0o40700)]);
        // A link in the selection itself is skipped too.
        let linked = plan(&[link], &grid, false, &ctx()).unwrap();
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
        let planned = plan(std::slice::from_ref(&top), &Grid::of_mode(0), true, &ctx()).unwrap();
        let report = chmod(&planned.pairs, &ctx());
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
        let report = chmod(&[Planned::item(a.clone(), 0o600)], &cancelled);
        assert!(report.cancelled);
        assert!(report.record.is_none(), "nothing happened, nothing to undo");
        assert_eq!(mode_of(&a), 0o644);
        assert!(matches!(
            plan(&[a], &Grid::of_mode(0), false, &cancelled),
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
            std::slice::from_ref(&top),
            &Grid::of_mode(0o600),
            true,
            &ctx(),
        )
        .unwrap();
        assert_eq!(planned.pairs.len(), 4);

        // A link to a file where the folder was.
        swap_for_link(&sub, &outside);
        let report = chmod(&planned.pairs, &ctx());
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
        let report = chmod(&planned.pairs, &ctx());
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
            std::slice::from_ref(&a),
            &Grid::of_mode(0o666),
            false,
            &ctx(),
        )
        .unwrap();
        std::fs::remove_file(&a).unwrap();
        std::os::unix::fs::symlink(&outside, &a).unwrap();
        let report = chmod(&planned.pairs, &ctx());
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
        let top = t.dir("tree/top");
        let sub = t.dir("tree/top/sub");
        let inner = t.file("tree/top/sub/inner.txt", b"i");
        let far = t.dir("far");
        let far_inner = t.file("far/inner.txt", b"f");
        set(&inner, 0o644);
        set(&top, 0o755);
        set(&sub, 0o755);
        let planned = plan(
            std::slice::from_ref(&top),
            &Grid::of_mode(0o600),
            true,
            &ctx(),
        )
        .unwrap();
        let record = chmod(&planned.pairs, &ctx()).record.unwrap();
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
        let through = t.symlink(t.join("real"), "shortcut").join("a.txt");
        let planned = plan(&[through], &Grid::of_mode(0o600), false, &ctx()).unwrap();
        let report = chmod(&planned.pairs, &ctx());
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
            std::slice::from_ref(&a),
            &Grid::of_mode(0o644),
            false,
            &ctx(),
        )
        .unwrap();
        let report = chmod(&planned.pairs, &ctx());
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(mode_of(&a), 0o644);
    }

    /// Without `/proc` there is no name for a descriptor, and the change
    /// says so rather than falling back to the path.
    #[test]
    fn without_proc_the_change_refuses_in_words() {
        assert!(
            ready_at(Path::new(PROC_FD)).is_ok(),
            "the test machine has /proc"
        );
        let why = ready_at(Path::new("/nonexistent/df-proc")).unwrap_err();
        assert!(why.contains("/proc is not mounted"), "{why}");
    }
}
