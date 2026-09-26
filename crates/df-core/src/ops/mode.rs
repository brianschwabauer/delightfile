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
//! tree, and never followed.
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
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

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
/// as `lstat` read it, file type and all.
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

/// What a change is about to do, worked out before any of it is done.
#[derive(Debug, Default)]
pub struct ModePlan {
    /// `(path, mode)` for [`chmod`], each mode the grid applied to that
    /// path's own, parents before what is in them.
    pub pairs: Vec<(PathBuf, u32)>,
    /// Links met and left alone.
    pub links: usize,
    /// Paths that could not be read: the item itself, or a folder the walk
    /// could not list.
    pub errors: Vec<(PathBuf, String)>,
}

/// Work out the new mode of every path the change is about: each of
/// `targets`, and with `recursive`, everything inside the folders among them
/// ([`searchable`] for every folder). Links are counted and skipped.
///
/// Walks with an explicit stack rather than by recursion, so a tree deep
/// enough to be a problem is a long `Vec` and not a stack overflow, and asks
/// `ctx` between entries: the only error is [`DfError::Cancelled`], and
/// everything the walk cannot read is in [`ModePlan::errors`].
pub fn plan(targets: &[PathBuf], grid: &Grid, recursive: bool, ctx: &TaskCtx) -> Result<ModePlan> {
    let mut plan = ModePlan::default();
    let mut folders: Vec<PathBuf> = Vec::new();
    for target in targets {
        ctx.checkpoint()?;
        match std::fs::symlink_metadata(target) {
            Err(e) => plan.errors.push((target.clone(), e.to_string())),
            Ok(meta) if meta.file_type().is_symlink() => plan.links += 1,
            Ok(meta) if recursive && meta.is_dir() => {
                plan.pairs
                    .push((target.clone(), searchable(grid.apply(meta.mode()))));
                folders.push(target.clone());
            }
            Ok(meta) => plan.pairs.push((target.clone(), grid.apply(meta.mode()))),
        }
    }
    while let Some(folder) = folders.pop() {
        let listing = match std::fs::read_dir(&folder) {
            Ok(listing) => listing,
            Err(e) => {
                plan.errors.push((folder, e.to_string()));
                continue;
            }
        };
        for entry in listing {
            ctx.checkpoint()?;
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(e) => {
                    plan.errors.push((folder.clone(), e.to_string()));
                    continue;
                }
            };
            match std::fs::symlink_metadata(&path) {
                Err(e) => plan.errors.push((path, e.to_string())),
                Ok(meta) if meta.file_type().is_symlink() => plan.links += 1,
                Ok(meta) if meta.is_dir() => {
                    plan.pairs
                        .push((path.clone(), searchable(grid.apply(meta.mode()))));
                    folders.push(path);
                }
                Ok(meta) => plan.pairs.push((path, grid.apply(meta.mode()))),
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
/// The nine bits come from `pairs`; the special bits are kept as the path
/// has them now, whatever the pair says. A path that is a link is skipped,
/// and one that already has its mode is left untouched and not recorded.
/// Failures are collected rather than returned, and a cancel stops between
/// two paths with everything set so far recorded, so a `u` takes back
/// exactly the part that happened.
pub fn chmod(pairs: &[(PathBuf, u32)], ctx: &TaskCtx) -> ModeReport {
    let mut report = ModeReport::default();
    // Each path as it is now: whether it is a folder decides the order, and
    // what its mode is now is the `before` of the record.
    let mut items: Vec<(&Path, bool, u32, u32)> = Vec::with_capacity(pairs.len());
    for (path, mode) in pairs {
        match std::fs::symlink_metadata(path) {
            Err(e) => report.errors.push((path.clone(), e.to_string())),
            Ok(meta) if meta.file_type().is_symlink() => report.links += 1,
            Ok(meta) => {
                let now = meta.mode();
                let target = (now & !PERMISSIONS) | (mode & PERMISSIONS);
                items.push((path, meta.is_dir(), now, target));
            }
        }
    }
    let order = safe_order(
        &items
            .iter()
            .map(|(path, is_dir, _, target)| (*path, *is_dir, *target))
            .collect::<Vec<_>>(),
    );
    let mut changes: Vec<(usize, ModeChange)> = Vec::new();
    for index in order {
        if ctx.checkpoint().is_err() {
            report.cancelled = true;
            break;
        }
        let (path, _, before, target) = items[index];
        if target == before {
            report.unchanged += 1;
            ctx.advance(0, 1);
            continue;
        }
        // The error without the path: whoever shows it names the file
        // (`op_toast`), and a path in both would say it twice.
        let set = std::fs::Permissions::from_mode(target & MODE_BITS);
        if let Err(e) = std::fs::set_permissions(path, set) {
            report.errors.push((path.to_path_buf(), e.to_string()));
            ctx.advance(0, 1);
            continue;
        }
        let after = std::fs::symlink_metadata(path)
            .map(|meta| meta.mode())
            .unwrap_or(target);
        report.changed += 1;
        if !report.unrecorded {
            changes.push((
                index,
                ModeChange {
                    path: path.to_path_buf(),
                    before,
                    after,
                },
            ));
            // Past the bound a record would be megabytes of paths held for a
            // `u` nobody expects to cover a whole disk: the change stands,
            // unrecorded, and the toast says so.
            if changes.len() > MAX_MANIFEST_ENTRIES {
                report.unrecorded = true;
                changes = Vec::new();
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
/// again, before it is restored. If a check or a restore fails after some
/// paths have gone back, the rest are handed back as the remainder, so a
/// second `u` finishes rather than refusing over the half that is right.
pub(super) fn undo(changes: &[ModeChange], ctx: &TaskCtx) -> UndoAttempt {
    let refuse = |e: DfError| UndoAttempt {
        result: Err(e),
        remaining: None,
    };
    if changes.is_empty() {
        return refuse(DfError::Op("nothing to undo".to_string()));
    }
    // The folders this change left shut to their owner: what is under one
    // cannot be looked at until the undo has opened it.
    let shut: HashSet<&Path> = changes
        .iter()
        .filter(|change| change.is_dir() && change.after & OWNER_SEARCH == 0)
        .map(|change| change.path.as_path())
        .collect();
    let mut checked = vec![false; changes.len()];
    for (index, change) in changes.iter().enumerate() {
        match std::fs::symlink_metadata(&change.path) {
            Ok(meta) => {
                if let Err(e) = still_as_left(change, meta.mode()) {
                    return refuse(e);
                }
                checked[index] = true;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
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
    for index in order {
        let change = &changes[index];
        let checked_now = if checked[index] {
            Ok(())
        } else {
            match std::fs::symlink_metadata(&change.path) {
                Ok(meta) => still_as_left(change, meta.mode()),
                Err(e) => Err(unreachable(change, &e)),
            }
        };
        let step = checked_now.and_then(|()| ctx.checkpoint()).and_then(|()| {
            std::fs::set_permissions(
                &change.path,
                std::fs::Permissions::from_mode(change.before & MODE_BITS),
            )
            .map_err(|e| DfError::io(&change.path, e))
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
fn still_as_left(change: &ModeChange, now: u32) -> Result<()> {
    if now & FILE_TYPE != change.after & FILE_TYPE {
        return Err(DfError::Op(format!(
            "cannot undo: {} is not the same kind of file any more",
            change.path.display()
        )));
    }
    if now != change.after {
        return Err(DfError::Op(format!(
            "cannot undo: the permissions of {} have changed since",
            change.path.display()
        )));
    }
    Ok(())
}

/// Why a recorded path cannot be looked at: gone, or out of reach.
fn unreachable(change: &ModeChange, e: &std::io::Error) -> DfError {
    if e.kind() == std::io::ErrorKind::NotFound {
        DfError::Op(format!(
            "cannot undo: {} is no longer there",
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
                (a.clone(), 0o755),
                (b.clone(), 0o640),
                (same.clone(), 0o640),
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
                    after: 0o100755
                },
                ModeChange {
                    path: b.clone(),
                    before: 0o100600,
                    after: 0o100640
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
        let report = chmod(&[(dir.clone(), 0o755)], &ctx());
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
        let report = chmod(&[(a.clone(), 0o600), (b.clone(), 0o600)], &ctx());
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
        let mut pairs = planned.pairs.clone();
        pairs.sort();
        let mut expected = vec![
            (top.clone(), 0o40755),
            (inner.clone(), 0o40755),
            (file.clone(), 0o100644),
            (deep.clone(), 0o100644),
        ];
        expected.sort();
        assert_eq!(pairs, expected);
        assert!(!pairs.iter().any(|(p, _)| p == &link || p == &dir_link));

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
        assert_eq!(flat.pairs, vec![(top.clone(), 0o40700)]);
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
        let report = chmod(&[(a.clone(), 0o600)], &cancelled);
        assert!(report.cancelled);
        assert!(report.record.is_none(), "nothing happened, nothing to undo");
        assert_eq!(mode_of(&a), 0o644);
        assert!(matches!(
            plan(&[a], &Grid::of_mode(0), false, &cancelled),
            Err(DfError::Cancelled)
        ));
    }
}
