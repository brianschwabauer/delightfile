//! Yank, cut and paste — `y`, `x`, `b`, `p`, `P`.
//!
//! The clipboard is a set of paths and a verb, nothing more. It is also the
//! *only* set of paths the program carries: `y` and `x` replace it, and `b`
//! adds to it or takes back out of it one gesture at a time, so a handful of
//! files gathered from four directories is a clipboard like any other (the
//! app's `tray` module tells the story of the second container this used to
//! be). The rules for that — [`Clipboard::toggle`] and the rest — live here
//! rather than in the app so they are tested without a window.
//!
//! The interesting part is [`plan_paste`]: given a clipboard and a
//! destination, it works out what each item would become, and hands back the
//! collisions **as data** for the UI to resolve with the side-by-side dialog
//! (PLAN §5). It never decides for the user. `P` — paste with force — is the
//! one exception, and even it cannot talk the safety rails out of anything.
//!
//! ## The rails
//!
//! Three ways a paste could destroy the thing it was copying, each refused:
//!
//! 1. **A directory into itself, or into its own descendant.** `cp -r a a/b`
//!    recurses until the disk is full. Refused before anything is written, and
//!    refused on the *resolved* paths: a destination spelled through a symlink
//!    is still inside the source.
//! 2. **Overwriting the source with its own copy.** Both the literal case
//!    (`src == dst`) and the sideways one, where the destination *contains* the
//!    source, so overwriting it would take the source with it. Device and inode
//!    are compared, not text, so a symlinked route to the same file is caught.
//! 3. **A name collision in the source's own directory** is not a conflict at
//!    all — it is a duplicate, and yazi's answer is the right one: `notes.txt`
//!    becomes `notes_1.txt`, no dialog.
//!
//! And one way the *undo* of a paste could: undoing a copy deletes what the
//! copy created, so a destination that was already there — overwritten, or a
//! directory merged into — is deliberately left out of the record. `u` after an
//! overwrite therefore does nothing rather than finishing off the file the
//! overwrite damaged.

use std::path::{Component, Path, PathBuf};

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::journal::{CopyManifest, MovedPath, OpRecord};
use super::trash::{suffixed, MAX_TRASH_COLLISIONS};
use super::{exists, file_name, is_ancestor_resolved, is_real_dir, is_url, normalize, same_file};

/// A path as the clipboard carries it: absolute and lexically clean
/// ([`normalize`]) — unless it is a URL, which is kept exactly as given.
///
/// `y` on a row of a remote pane carries that row's `sftp://host/…` display
/// path, and everything downstream — [`crate::vfs::VfsPath::parse`], the
/// download a `p` starts, a sync's `rsync` — reads the URL. Normalized, it was
/// `<cwd>/sftp:/host/…`: a local path to nothing, which a `p` then refused as
/// missing and a sync treated as local.
fn carried(path: &Path) -> PathBuf {
    if is_url(path) {
        path.to_path_buf()
    } else {
        normalize(path)
    }
}

/// What `p` will do with the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasteMode {
    /// `y` — the sources stay where they are.
    #[default]
    Copy,
    /// `x` — the sources move, and the clipboard is spent afterwards.
    Cut,
}

/// The yanked or cut set. `X` unyanks by clearing it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clipboard {
    pub mode: PasteMode,
    /// The carried paths, in the order they were picked up.
    ///
    /// Insertion order, not sorted: a pile built with `b` is a record of *what
    /// you picked*, and re-ordering it under the user would break the one thing
    /// a person tracks about a pile they made by hand — that the last thing
    /// they added is at the bottom.
    pub paths: Vec<PathBuf>,
}

/// What one `b` press did, so the toast can say it.
///
/// At most one of the two is non-zero: [`Clipboard::toggle`] moves a whole
/// batch the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Toggled {
    pub added: usize,
    pub removed: usize,
}

impl Clipboard {
    /// `y`: copy on paste.
    pub fn yank(paths: impl IntoIterator<Item = PathBuf>) -> Clipboard {
        Clipboard {
            mode: PasteMode::Copy,
            paths: paths.into_iter().map(|p| carried(&p)).collect(),
        }
    }

    /// `x`: move on paste.
    pub fn cut(paths: impl IntoIterator<Item = PathBuf>) -> Clipboard {
        Clipboard {
            mode: PasteMode::Cut,
            paths: paths.into_iter().map(|p| carried(&p)).collect(),
        }
    }

    /// `X`.
    pub fn clear(&mut self) {
        self.paths.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether `path` is being carried, however it is spelled.
    pub fn contains(&self, path: &Path) -> bool {
        let path = carried(path);
        self.paths.contains(&path)
    }

    /// `b` over a selection, or over one row: add to the clipboard, or take
    /// back out of it.
    ///
    /// The gesture is a *toggle*, and the whole batch goes the same way: if
    /// every path offered is already carried the press takes them all out, and
    /// otherwise it puts the missing ones in. Deciding per path would mean a
    /// `b` on a selection of five, three of which are already in, doing two
    /// different things at once — and the toast could only say one of them.
    ///
    /// The verb is kept: adding to a cut adds to the cut, because the chip and
    /// the row marks already say which one this is, and a `b` that quietly
    /// turned a cut into a copy would change what the next `p` does. An empty
    /// clipboard has no verb worth keeping — `X` leaves the old one behind — so
    /// a pile started from nothing is a copy, the verb that cannot lose a file.
    pub fn toggle(&mut self, paths: &[PathBuf]) -> Toggled {
        let offered: Vec<PathBuf> = paths.iter().map(|p| carried(p)).collect();
        if offered.is_empty() {
            return Toggled::default();
        }
        // Sets for the membership questions: the cap the old basket had is
        // gone, so a `b` after `Ctrl+a` on a big directory must not be
        // quadratic in it.
        let mut held: std::collections::HashSet<PathBuf> = self.paths.iter().cloned().collect();
        if offered.iter().all(|path| held.contains(path)) {
            let offered: std::collections::HashSet<PathBuf> = offered.into_iter().collect();
            let before = self.paths.len();
            self.paths.retain(|path| !offered.contains(path));
            return Toggled {
                added: 0,
                removed: before - self.paths.len(),
            };
        }
        if self.paths.is_empty() {
            self.mode = PasteMode::Copy;
        }
        let mut added = 0;
        for path in offered {
            if held.insert(path.clone()) {
                self.paths.push(path);
                added += 1;
            }
        }
        Toggled { added, removed: 0 }
    }

    /// Take one path out, by its place in the list — the tray's `×`.
    pub fn remove(&mut self, index: usize) -> Option<PathBuf> {
        (index < self.paths.len()).then(|| self.paths.remove(index))
    }

    /// Whether the paths live in more than one directory.
    ///
    /// `y` and `x` only ever take rows from the listing on screen, so a
    /// clipboard that spans directories was built on purpose with `b` — which
    /// is what makes it worth a word before a `y` replaces it.
    pub fn spans_directories(&self) -> bool {
        let mut parents = self.paths.iter().map(|path| path.parent());
        let Some(first) = parents.next() else {
            return false;
        };
        parents.any(|parent| parent != first)
    }
}

/// One item the paste is ready to carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasteItem {
    pub src: PathBuf,
    pub dst: PathBuf,
    /// Set by `--force`, or by the user answering "overwrite" in the dialog.
    pub overwrite: bool,
}

/// A destination that is already taken, for the dialog to resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub src: PathBuf,
    pub dst: PathBuf,
    /// The free name a "rename" answer would use, so the dialog has a default
    /// to offer rather than an empty text field.
    pub suggested: PathBuf,
}

/// The user's answer to one conflict (or, with "apply to all", to every one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Overwrite,
    Skip,
    /// Keep both, under this name — a **plain file name**, never a path.
    ///
    /// The dialog's text field holds a name, and a name is what lands in the
    /// destination directory. Anything with a `/` in it, `.`, `..`, an empty
    /// string or an absolute path is refused by [`PastePlan::resolve`]: a
    /// relative multi-component answer used to be joined against the *process
    /// working directory*, so a paste could quietly write somewhere the user
    /// was not even looking at.
    Rename(PathBuf),
}

/// The name a rename answer must be: one ordinary path component.
fn rename_name(name: &Path) -> Result<&std::ffi::OsStr> {
    let mut components = name.components();
    let (Some(Component::Normal(first)), None) = (components.next(), components.next()) else {
        return Err(DfError::Op(format!(
            "\"{}\" is not a usable name: type a file name, not a path",
            name.display()
        )));
    };
    Ok(first)
}

/// What a paste would do, before it does any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastePlan {
    pub mode: PasteMode,
    pub dest_dir: PathBuf,
    pub ready: Vec<PasteItem>,
    pub conflicts: Vec<Conflict>,
    /// Cut items whose destination is where they already are: nothing to do,
    /// and not worth a dialog.
    pub no_ops: Vec<PathBuf>,
}

impl PastePlan {
    /// Is there anything left for the UI to ask about?
    pub fn is_settled(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// Answer one conflict, by source path. Unknown sources are ignored, since
    /// a dialog answering a conflict that has already been resolved is a race,
    /// not an error.
    ///
    /// Errors only on a [`Resolution::Rename`] carrying something that is not a
    /// plain file name; the conflict is then left unanswered, so the dialog can
    /// show the message and ask again.
    pub fn resolve(&mut self, src: &Path, answer: &Resolution) -> Result<()> {
        if let Resolution::Rename(name) = answer {
            rename_name(name)?;
        }
        let Some(index) = self.conflicts.iter().position(|c| c.src == src) else {
            return Ok(());
        };
        let conflict = self.conflicts.remove(index);
        self.apply_one(conflict, answer);
        Ok(())
    }

    /// The dialog's "apply to all" — the same answer for every remaining
    /// conflict.
    ///
    /// A single explicit rename cannot be applied to many files without
    /// colliding, so "rename all" means "keep both, auto-named": every
    /// conflict's own suggested name is used and the name in the answer is
    /// ignored, which is why this one cannot fail.
    pub fn resolve_all(&mut self, answer: &Resolution) {
        for conflict in std::mem::take(&mut self.conflicts) {
            match answer {
                Resolution::Rename(_) => {
                    let dst = conflict.suggested.clone();
                    self.ready.push(PasteItem {
                        src: conflict.src,
                        dst,
                        overwrite: false,
                    });
                }
                other => self.apply_one(conflict, other),
            }
        }
    }

    fn apply_one(&mut self, conflict: Conflict, answer: &Resolution) {
        match answer {
            Resolution::Skip => {}
            Resolution::Overwrite => self.ready.push(PasteItem {
                src: conflict.src,
                dst: conflict.dst,
                overwrite: true,
            }),
            Resolution::Rename(name) => {
                // Always a name in the destination directory. `resolve` has
                // already refused anything that is not one.
                let Ok(name) = rename_name(name) else { return };
                self.ready.push(PasteItem {
                    src: conflict.src,
                    dst: self.dest_dir.join(name),
                    overwrite: false,
                });
            }
        }
    }
}

/// Work out what pasting `clip` into `dest_dir` would do.
///
/// Errors — as opposed to conflicts — are for the cases no answer can make
/// safe: a destination that is not a directory, and the two self-destruction
/// rails. Everything else comes back as data.
pub fn plan_paste(clip: &Clipboard, dest_dir: &Path, force: bool) -> Result<PastePlan> {
    let dest_dir = carried(dest_dir);
    if !exists(&dest_dir) {
        return Err(DfError::Op(format!(
            "{} does not exist",
            dest_dir.display()
        )));
    }
    if !dest_dir.is_dir() {
        return Err(DfError::Op(format!(
            "{} is not a directory",
            dest_dir.display()
        )));
    }

    let mut plan = PastePlan {
        mode: clip.mode,
        dest_dir: dest_dir.clone(),
        ready: Vec::new(),
        conflicts: Vec::new(),
        no_ops: Vec::new(),
    };
    // Names claimed by earlier items in this same paste, so two sources called
    // `notes.txt` from different directories do not both get the same slot.
    let mut claimed: Vec<PathBuf> = Vec::new();

    for src in &clip.paths {
        let src = carried(src);
        if !exists(&src) {
            return Err(DfError::io(
                &src,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
            ));
        }
        // Rail 1: a directory may not be pasted into itself or below itself.
        // Resolved, not lexical: a destination spelled through a symlink is
        // still inside the source, and the copy would recurse forever.
        if is_real_dir(&src) && is_ancestor_resolved(&src, &dest_dir) {
            return Err(DfError::Op(format!(
                "cannot paste {} into {}: that is inside itself",
                src.display(),
                dest_dir.display()
            )));
        }

        let dst = dest_dir.join(file_name(&src)?);

        if same_file(&src, &dst) {
            match clip.mode {
                // Pasting a yank into the directory it came from is how you
                // duplicate a file: yazi names the copy `name_1`.
                PasteMode::Copy => {
                    let dst = unique_name(&dest_dir, file_name(&src)?, &claimed)?;
                    claimed.push(dst.clone());
                    plan.ready.push(PasteItem {
                        src,
                        dst,
                        overwrite: false,
                    });
                }
                // Cutting and pasting into the same directory is nothing.
                PasteMode::Cut => plan.no_ops.push(src),
            }
            continue;
        }

        // A name another item of *this same paste* has already spoken for is
        // not a conflict — the user asked for both, and neither has landed yet,
        // so the second one is auto-named exactly like a duplicate.
        if !exists(&dst) && claimed.contains(&dst) {
            let dst = unique_name(&dest_dir, file_name(&src)?, &claimed)?;
            claimed.push(dst.clone());
            plan.ready.push(PasteItem {
                src,
                dst,
                overwrite: false,
            });
            continue;
        }
        if !exists(&dst) {
            claimed.push(dst.clone());
            plan.ready.push(PasteItem {
                src,
                dst,
                overwrite: false,
            });
            continue;
        }

        // Rail 2: never overwrite the source with its own copy — either
        // directly, or by clobbering a directory the source lives in.
        let devours_source = same_file(&dst, &src) || is_ancestor_resolved(&dst, &src);
        if force && !devours_source {
            claimed.push(dst.clone());
            plan.ready.push(PasteItem {
                src,
                dst,
                overwrite: true,
            });
            continue;
        }
        if force && devours_source {
            return Err(DfError::Op(format!(
                "refusing to overwrite {} with {}: it would destroy the source",
                dst.display(),
                src.display()
            )));
        }

        let suggested = unique_name(&dest_dir, file_name(&src)?, &claimed)?;
        plan.conflicts.push(Conflict {
            src,
            dst,
            suggested,
        });
    }

    Ok(plan)
}

/// The first free `name`, `name_1`, `name_2`… in `dir`.
///
/// `claimed` holds names this same paste has already spoken for but not yet
/// created.
pub fn unique_name(dir: &Path, name: &std::ffi::OsStr, claimed: &[PathBuf]) -> Result<PathBuf> {
    let bare = dir.join(name);
    if !exists(&bare) && !claimed.contains(&bare) {
        return Ok(bare);
    }
    for n in 1..MAX_TRASH_COLLISIONS {
        let candidate = dir.join(suffixed(name, n));
        if !exists(&candidate) && !claimed.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(DfError::Op(format!(
        "{}: no free name in {}",
        name.to_string_lossy(),
        dir.display()
    )))
}

/// What a paste actually did.
///
/// Per-item failures are collected rather than thrown, because a 200-file paste
/// that hits one permission-denied should still move the other 199 — and the
/// journal still needs a record of what moved. `cancelled` is the same idea:
/// what landed before the user pressed cancel is real, and is undoable.
#[derive(Debug, Clone, Default)]
pub struct PasteReport {
    /// The inverse of what was done, ready for the journal. `None` when nothing
    /// happened at all.
    pub record: Option<OpRecord>,
    pub copied: Vec<PathBuf>,
    pub moved: Vec<(PathBuf, PathBuf)>,
    pub skipped: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

/// Carry out a settled plan.
///
/// Conflicts still in the plan are treated as "skip": the UI resolves them
/// before calling, and a leftover conflict must never become a silent
/// overwrite.
pub fn execute(plan: &PastePlan, ctx: &TaskCtx) -> Result<PasteReport> {
    let mut report = PasteReport {
        skipped: plan
            .conflicts
            .iter()
            .map(|c| c.src.clone())
            .chain(plan.no_ops.iter().cloned())
            .collect(),
        ..Default::default()
    };

    // Measure first, so the bar is honest from the first byte rather than
    // growing its own total as it goes.
    let mut bytes_total = 0;
    let mut files_total = 0;
    for item in &plan.ready {
        if let Ok((b, f)) = super::copy::measure(&item.src) {
            bytes_total += b;
            files_total += f;
        }
    }
    ctx.set_total(bytes_total, files_total);

    let mut created: Vec<CopyManifest> = Vec::new();
    let mut moves: Vec<MovedPath> = Vec::new();

    for item in &plan.ready {
        if ctx.is_cancelled() {
            report.cancelled = true;
            break;
        }
        // Whether this destination is the copy's to take back, decided *before*
        // the copy runs. Undo of a copy deletes what the copy created; a
        // destination that was already there is not that. Overwriting a file
        // destroyed the old one, and merging into a directory left the old
        // contents in place — deleting either on `u` would be the undo
        // finishing the job rather than reversing it.
        let fresh = !exists(&item.dst);
        let outcome = match plan.mode {
            PasteMode::Copy => {
                super::copy::copy_tree(&item.src, &item.dst, ctx, item.overwrite).map(|_stats| ())
            }
            PasteMode::Cut => super::copy::move_path(&item.src, &item.dst, ctx, item.overwrite),
        };
        match outcome {
            Ok(()) => match plan.mode {
                PasteMode::Copy => {
                    if fresh {
                        // Recorded now, item by item: what is at `item.dst` is
                        // exactly what this copy just made there, so a paste
                        // cancelled after this item manifests this item whole
                        // and the ones it never started not at all.
                        match CopyManifest::of_tree(&item.dst) {
                            Ok(manifest) => created.push(manifest),
                            // The copy landed but cannot be manifested — too
                            // big to record, or unreadable. Leaving it out of
                            // the journal is the safe half: undo never deletes
                            // what it could not describe first.
                            Err(e) => log::warn!("no undo record for {}: {e}", item.dst.display()),
                        }
                    } else {
                        log::info!(
                            "no undo record for {}: it was already there",
                            item.dst.display()
                        );
                    }
                    report.copied.push(item.dst.clone());
                }
                PasteMode::Cut => {
                    match MovedPath::record(&item.src, &item.dst) {
                        Ok(m) => moves.push(m),
                        Err(e) => log::warn!("no undo record for {}: {e}", item.dst.display()),
                    }
                    report.moved.push((item.src.clone(), item.dst.clone()));
                }
            },
            Err(DfError::Cancelled) => {
                report.cancelled = true;
                break;
            }
            Err(e) => report.errors.push((item.src.clone(), e.to_string())),
        }
    }

    report.record = match plan.mode {
        PasteMode::Copy if !created.is_empty() => Some(OpRecord::Copy { created }),
        PasteMode::Cut if !moves.is_empty() => Some(OpRecord::Move { moves }),
        _ => None,
    };
    Ok(report)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::{gnarly_names, TempTree};
    use std::ffi::OsStr;

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    #[test]
    fn a_yanked_url_is_carried_exactly_as_given() {
        let remote = PathBuf::from("sftp://host/a");
        let local = PathBuf::from("/home/brian/./b/../c");
        for clip in [
            Clipboard::yank([remote.clone(), local.clone()]),
            Clipboard::cut([remote.clone(), local.clone()]),
        ] {
            assert_eq!(clip.paths, [remote.clone(), PathBuf::from("/home/brian/c")]);
            assert!(clip.contains(&remote));
            assert!(clip.contains(Path::new("/home/brian/c")));
        }
        // `b` on a remote row adds the URL, and a second `b` takes it back.
        let mut clip = Clipboard::default();
        assert_eq!(clip.toggle(std::slice::from_ref(&remote)).added, 1);
        assert_eq!(clip.paths, std::slice::from_ref(&remote));
        assert_eq!(clip.toggle(std::slice::from_ref(&remote)).removed, 1);
        // A URL and its neighbour share a parent, so one listing's pile does
        // not read as spanning directories.
        let pile = Clipboard::yank([remote, PathBuf::from("sftp://host/b")]);
        assert!(!pile.spans_directories());
    }

    #[test]
    fn a_plain_paste_has_no_conflicts() {
        let t = TempTree::new("paste-plain");
        let a = t.file("src/a.txt", b"a");
        let b = t.file("src/b.txt", b"b");
        let dest = t.dir("dst");

        let clip = Clipboard::yank([a.clone(), b.clone()]);
        let plan = plan_paste(&clip, &dest, false).unwrap();
        assert!(plan.is_settled());
        assert_eq!(plan.ready.len(), 2);

        let report = execute(&plan, &ctx()).unwrap();
        assert_eq!(report.copied.len(), 2);
        assert!(report.errors.is_empty());
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(&a).unwrap(), b"a", "a yank leaves the source");
        assert!(matches!(report.record, Some(OpRecord::Copy { .. })));
    }

    #[test]
    fn a_cut_paste_moves_and_journals_a_move() {
        let t = TempTree::new("paste-cut");
        let a = t.file("src/a.txt", b"a");
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::cut([a.clone()]), &dest, false).unwrap();
        let report = execute(&plan, &ctx()).unwrap();
        assert!(!exists(&a));
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"a");
        assert_eq!(report.moved.len(), 1);
        assert!(matches!(report.record, Some(OpRecord::Move { .. })));
    }

    #[test]
    fn pasting_a_directory_into_itself_is_refused() {
        let t = TempTree::new("paste-self");
        let dir = t.dir("project");
        let clip = Clipboard::yank([dir.clone()]);
        let err = plan_paste(&clip, &dir, false).unwrap_err();
        assert!(err.to_string().contains("inside itself"), "{err}");
    }

    #[test]
    fn pasting_a_directory_into_its_own_descendant_is_refused() {
        let t = TempTree::new("paste-descendant");
        let dir = t.dir("project");
        let deep = t.dir("project/a/b/c");
        let clip = Clipboard::yank([dir.clone()]);
        let err = plan_paste(&clip, &deep, false).unwrap_err();
        assert!(err.to_string().contains("inside itself"), "{err}");

        // The same for a cut.
        let err = plan_paste(&Clipboard::cut([dir]), &deep, true).unwrap_err();
        assert!(err.to_string().contains("inside itself"), "{err}");
    }

    #[test]
    fn a_symlinked_destination_is_still_inside_the_source() {
        // BUG: rail 1 compared paths lexically, so a destination spelled
        // through a symlink read as "somewhere else" — and the copy recursed
        // into its own output until the disk filled.
        let t = TempTree::new("paste-symlink-self");
        let dir = t.dir("project");
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        // `link` is another spelling of `project`, so pasting `project` into
        // `link` is pasting a directory into itself by a different name.
        let link = t.symlink(&dir, "link");
        let err = plan_paste(&Clipboard::yank([dir.clone()]), &link, false).unwrap_err();
        assert!(err.to_string().contains("inside itself"), "{err}");
    }

    #[test]
    fn a_symlinked_destination_below_the_source_is_refused() {
        // BUG: as above, one level down.
        let t = TempTree::new("paste-symlink-descendant");
        let dir = t.dir("project");
        let deep = t.dir("project/a/b");
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let link = t.symlink(&deep, "link");
        let err = plan_paste(&Clipboard::yank([dir.clone()]), &link, false).unwrap_err();
        assert!(err.to_string().contains("inside itself"), "{err}");
    }

    #[test]
    fn a_file_may_be_pasted_into_a_directory_that_holds_it() {
        // The rail is about directories eating themselves; a *file* going into
        // its own directory is the duplicate case, not an error.
        let t = TempTree::new("paste-file-own-dir");
        let dir = t.dir("d");
        let f = t.file("d/a.txt", b"x");
        let plan = plan_paste(&Clipboard::yank([f]), &dir, false).unwrap();
        assert_eq!(plan.ready.len(), 1);
        assert_eq!(plan.ready[0].dst, dir.join("a_1.txt"));
    }

    #[test]
    fn same_directory_copy_auto_suffixes() {
        let t = TempTree::new("paste-suffix");
        let dir = t.dir("d");
        let f = t.file("d/notes.txt", b"body");

        for expected in ["notes_1.txt", "notes_2.txt", "notes_3.txt"] {
            let plan = plan_paste(&Clipboard::yank([f.clone()]), &dir, false).unwrap();
            assert!(plan.is_settled(), "a duplicate is never a conflict");
            assert_eq!(plan.ready[0].dst, dir.join(expected));
            execute(&plan, &ctx()).unwrap();
            assert_eq!(std::fs::read(dir.join(expected)).unwrap(), b"body");
        }
    }

    #[test]
    fn a_cut_onto_a_hardlink_of_the_source_reports_itself_skipped() {
        let t = TempTree::new("paste-hardlink");
        let src = t.file("src/notes.txt", b"body");
        let dest = t.dir("dst");
        // The same inode under two names, in two directories.
        std::fs::hard_link(&src, dest.join("notes.txt")).unwrap();

        let plan = plan_paste(&Clipboard::cut([src.clone()]), &dest, false).unwrap();
        let report = execute(&plan, &ctx()).unwrap();
        assert!(
            !exists(&src) || !report.skipped.is_empty(),
            "a cut that did nothing must say so rather than claim to have moved: \
             plan {plan:?} report {report:?}"
        );
    }

    #[test]
    fn same_directory_cut_is_a_no_op() {
        let t = TempTree::new("paste-cut-same");
        let dir = t.dir("d");
        let f = t.file("d/notes.txt", b"body");
        let plan = plan_paste(&Clipboard::cut([f.clone()]), &dir, false).unwrap();
        assert!(plan.ready.is_empty());
        assert_eq!(plan.no_ops, vec![normalize(&f)]);
        let report = execute(&plan, &ctx()).unwrap();
        assert!(report.record.is_none());
        assert_eq!(std::fs::read(&f).unwrap(), b"body");
    }

    #[test]
    fn a_collision_becomes_a_conflict_with_a_suggestion() {
        let t = TempTree::new("paste-conflict");
        let src = t.file("src/notes.txt", b"new");
        let dest = t.dir("dst");
        std::fs::write(dest.join("notes.txt"), b"old").unwrap();

        let plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
        assert!(!plan.is_settled());
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].dst, dest.join("notes.txt"));
        assert_eq!(plan.conflicts[0].suggested, dest.join("notes_1.txt"));

        // Nothing happens while the conflict stands.
        let report = execute(&plan, &ctx()).unwrap();
        assert!(report.copied.is_empty());
        assert_eq!(report.skipped, vec![normalize(&src)]);
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"old");
    }

    #[test]
    fn resolving_a_conflict_by_overwriting() {
        let t = TempTree::new("paste-overwrite");
        let src = t.file("src/notes.txt", b"new");
        let dest = t.dir("dst");
        std::fs::write(dest.join("notes.txt"), b"old").unwrap();

        let mut plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
        plan.resolve(&normalize(&src), &Resolution::Overwrite)
            .unwrap();
        assert!(plan.is_settled());
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"new");
    }

    #[test]
    fn resolving_a_conflict_by_skipping_or_renaming() {
        let t = TempTree::new("paste-skip-rename");
        let src = t.file("src/notes.txt", b"new");
        let dest = t.dir("dst");
        std::fs::write(dest.join("notes.txt"), b"old").unwrap();

        let mut plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
        plan.resolve(&normalize(&src), &Resolution::Skip).unwrap();
        assert!(plan.ready.is_empty());
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"old");

        let mut plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
        let suggested = plan.conflicts[0].suggested.clone();
        let name = PathBuf::from(suggested.file_name().unwrap());
        plan.resolve(&normalize(&src), &Resolution::Rename(name))
            .unwrap();
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(&suggested).unwrap(), b"new");
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"old");
    }

    #[test]
    fn a_rename_answer_must_be_a_plain_file_name() {
        // BUG: a multi-component or absolute rename answer was used as the
        // destination path verbatim. A relative one — "notes/copy.txt", or a
        // stray "../notes.txt" — was therefore resolved against the *process
        // working directory*, so answering the dialog wrote somewhere the user
        // was not looking at, and outside the directory they were pasting into.
        let t = TempTree::new("paste-rename-path");
        let src = t.file("src/notes.txt", b"new");
        let dest = t.dir("dst");
        std::fs::write(dest.join("notes.txt"), b"old").unwrap();

        for bad in [
            "sub/notes.txt",
            "../notes.txt",
            "/tmp/notes.txt",
            ".",
            "..",
            "",
        ] {
            let mut plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
            let err = plan
                .resolve(&normalize(&src), &Resolution::Rename(PathBuf::from(bad)))
                .unwrap_err();
            assert!(
                err.to_string().contains("not a usable name"),
                "{bad:?}: {err}"
            );
            assert_eq!(
                plan.conflicts.len(),
                1,
                "{bad:?} left the conflict standing"
            );
            assert!(plan.ready.is_empty(), "{bad:?}");
        }

        // The plain name it should have been all along.
        let mut plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
        plan.resolve(
            &normalize(&src),
            &Resolution::Rename(PathBuf::from("kept.txt")),
        )
        .unwrap();
        assert_eq!(plan.ready[0].dst, dest.join("kept.txt"));
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("kept.txt")).unwrap(), b"new");
    }

    #[test]
    fn resolve_all_applies_to_every_conflict() {
        let t = TempTree::new("paste-apply-all");
        let dest = t.dir("dst");
        let mut sources = Vec::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            sources.push(t.file(Path::new("src").join(name), b"new"));
            std::fs::write(dest.join(name), b"old").unwrap();
        }
        let mut plan = plan_paste(&Clipboard::yank(sources), &dest, false).unwrap();
        assert_eq!(plan.conflicts.len(), 3);
        plan.resolve_all(&Resolution::Overwrite);
        assert!(plan.is_settled());
        execute(&plan, &ctx()).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert_eq!(std::fs::read(dest.join(name)).unwrap(), b"new");
        }
    }

    #[test]
    fn resolve_all_with_rename_keeps_both_of_each() {
        let t = TempTree::new("paste-rename-all");
        let dest = t.dir("dst");
        let mut sources = Vec::new();
        for name in ["a.txt", "b.txt"] {
            sources.push(t.file(Path::new("src").join(name), b"new"));
            std::fs::write(dest.join(name), b"old").unwrap();
        }
        let mut plan = plan_paste(&Clipboard::yank(sources), &dest, false).unwrap();
        plan.resolve_all(&Resolution::Rename(PathBuf::from("ignored")));
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("a_1.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(dest.join("b_1.txt")).unwrap(), b"new");
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"old");
    }

    #[test]
    fn force_overwrites_without_asking() {
        let t = TempTree::new("paste-force");
        let src = t.file("src/notes.txt", b"new");
        let dest = t.dir("dst");
        std::fs::write(dest.join("notes.txt"), b"old").unwrap();

        let plan = plan_paste(&Clipboard::yank([src]), &dest, true).unwrap();
        assert!(plan.is_settled());
        assert!(plan.ready[0].overwrite);
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"new");
    }

    #[test]
    fn force_cannot_overwrite_the_source_with_its_own_copy() {
        let t = TempTree::new("paste-force-self");
        let dir = t.dir("d");
        let f = t.file("d/notes.txt", b"body");
        // Reach the same file by a symlinked route, so the destination path is
        // spelled differently but is the same inode.
        t.symlink(&dir, "link");
        let via_link = t.join("link");

        let plan = plan_paste(&Clipboard::yank([f.clone()]), &via_link, true).unwrap();
        // Same file, so this is the duplicate case, not an overwrite.
        assert!(plan.ready[0].dst.file_name().unwrap() != OsStr::new("notes.txt"));
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"body", "the source survives");
    }

    #[test]
    fn force_cannot_overwrite_a_directory_containing_the_source() {
        let t = TempTree::new("paste-force-devour");
        // Copying `dst/inner` up into `parent`, where a directory called
        // `inner` — the one holding the source — already lives.
        let parent = t.dir("parent");
        let inner = t.dir("parent/inner");
        let src = t.file("parent/inner/inner", b"x");
        assert!(src.is_file());

        let clip = Clipboard::yank([src.clone()]);
        let err = plan_paste(&clip, &parent, true).unwrap_err();
        assert!(err.to_string().contains("destroy the source"), "{err}");
        assert!(inner.is_dir(), "nothing was touched");
    }

    #[test]
    fn undo_of_an_overwriting_copy_does_not_delete_the_old_destination() {
        // BUG: an overwriting paste journalled the destination as something the
        // copy "created", so `u` deleted it outright — the file the user chose
        // to overwrite was not put back, it was finished off. For a directory,
        // which an overwrite *merges* into, `u` took the whole pre-existing
        // tree with it.
        let t = TempTree::new("paste-undo-overwrite");
        let src = t.dir("src/photos");
        std::fs::write(src.join("new.jpg"), b"new").unwrap();
        let dest = t.dir("dst");
        let victim = t.dir("dst/photos");
        std::fs::write(victim.join("wedding.jpg"), b"irreplaceable").unwrap();

        let plan = plan_paste(&Clipboard::yank([src]), &dest, true).unwrap();
        assert!(plan.ready[0].overwrite);
        let report = execute(&plan, &ctx()).unwrap();
        assert!(victim.join("new.jpg").is_file(), "the paste landed");

        if let Some(record) = report.record {
            super::super::journal::undo_record(&record, &ctx()).unwrap();
        }
        assert_eq!(
            std::fs::read(victim.join("wedding.jpg")).unwrap(),
            b"irreplaceable",
            "undo may only remove what the copy created"
        );
    }

    #[test]
    fn undo_of_an_overwriting_file_copy_leaves_the_file_there() {
        let t = TempTree::new("paste-undo-overwrite-file");
        let src = t.file("src/notes.txt", b"new");
        let dest = t.dir("dst");
        let victim = t.file("dst/notes.txt", b"the old contents");

        let plan = plan_paste(&Clipboard::yank([src]), &dest, true).unwrap();
        let report = execute(&plan, &ctx()).unwrap();
        if let Some(record) = report.record {
            super::super::journal::undo_record(&record, &ctx()).unwrap();
        }
        assert!(
            exists(&victim),
            "undo of an overwrite must not leave the user with nothing at all"
        );
    }

    #[test]
    fn two_sources_with_the_same_name_do_not_collide_with_each_other() {
        let t = TempTree::new("paste-two-same");
        let a = t.file("one/notes.txt", b"one");
        let b = t.file("two/notes.txt", b"two");
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::yank([a, b]), &dest, false).unwrap();
        assert!(plan.is_settled());
        assert_eq!(plan.ready[0].dst, dest.join("notes.txt"));
        assert_eq!(plan.ready[1].dst, dest.join("notes_1.txt"));
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"one");
        assert_eq!(std::fs::read(dest.join("notes_1.txt")).unwrap(), b"two");
    }

    #[test]
    fn pastes_a_tree_of_gnarly_names() {
        let t = TempTree::new("paste-gnarly");
        let src = t.dir("src");
        for name in gnarly_names() {
            std::fs::write(src.join(&name), name.as_bytes()).unwrap();
        }
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::yank([src]), &dest, false).unwrap();
        execute(&plan, &ctx()).unwrap();
        for name in gnarly_names() {
            assert_eq!(
                std::fs::read(dest.join("src").join(&name)).unwrap(),
                name.as_bytes(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_missing_source_is_an_error_not_a_conflict() {
        let t = TempTree::new("paste-missing");
        let dest = t.dir("dst");
        let clip = Clipboard::yank([t.join("gone.txt")]);
        assert!(plan_paste(&clip, &dest, false).is_err());
    }

    #[test]
    fn a_destination_that_is_not_a_directory_is_an_error() {
        let t = TempTree::new("paste-dest");
        let f = t.file("a.txt", b"x");
        let clip = Clipboard::yank([f.clone()]);
        let err = plan_paste(&clip, &f, false).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
        let err = plan_paste(&clip, &t.join("nope"), false).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn one_failure_does_not_stop_the_rest() {
        let t = TempTree::new("paste-partial");
        let good = t.file("src/good.txt", b"g");
        let doomed = t.file("src/doomed.txt", b"d");
        let dest = t.dir("dst");
        let mut plan = plan_paste(
            &Clipboard::yank([doomed.clone(), good.clone()]),
            &dest,
            false,
        )
        .unwrap();
        // Make the first item fail by deleting it after planning.
        std::fs::remove_file(&doomed).unwrap();
        plan.ready.sort_by_key(|i| i.src.clone());

        let report = execute(&plan, &ctx()).unwrap();
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.copied.len(), 1);
        assert!(dest.join("good.txt").is_file());
        assert!(matches!(report.record, Some(OpRecord::Copy { .. })));
    }

    #[test]
    fn cancelling_stops_the_paste_but_keeps_the_record() {
        let t = TempTree::new("paste-cancel");
        let a = t.file("src/a.txt", b"a");
        let b = t.file("src/b.txt", b"b");
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::yank([a, b]), &dest, false).unwrap();

        let ctx = TaskCtx::detached();
        ctx.flags().cancel();
        let report = execute(&plan, &ctx).unwrap();
        assert!(report.cancelled);
        assert!(report.copied.is_empty());
        assert!(report.record.is_none());
    }

    #[test]
    fn a_cancelled_paste_manifests_exactly_what_it_created() {
        use crate::tasks::{ProgressSink, TaskFlags};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;

        /// Cancel the moment the first *file* has landed whole, so the paste
        /// stops between items rather than at a guessed instant.
        struct CancelAfterFirst {
            flags: Arc<TaskFlags>,
            seen: AtomicU64,
        }
        impl ProgressSink for CancelAfterFirst {
            fn set_total(&self, _bytes: u64, _files: u64) {}
            fn advance(&self, _bytes: u64, files: u64) {
                if self.seen.fetch_add(files, Ordering::SeqCst) + files > 0 {
                    self.flags.cancel();
                }
            }
        }

        let t = TempTree::new("paste-cancel-manifest");
        let a = t.file("src/a.txt", b"aaaa");
        let b = t.file("src/b.txt", b"bbbb");
        let dest = t.dir("dst");
        let plan = plan_paste(&Clipboard::yank([a, b]), &dest, false).unwrap();

        let flags = Arc::new(TaskFlags::new());
        let ctx = TaskCtx::with_sink(
            Arc::clone(&flags),
            Arc::new(CancelAfterFirst {
                flags: Arc::clone(&flags),
                seen: AtomicU64::new(0),
            }),
        );
        let report = execute(&plan, &ctx).unwrap();
        assert!(report.cancelled);
        assert_eq!(report.copied.len(), 1, "only the first item landed");

        // The record covers that one item and nothing else, and undoing it
        // takes back exactly what the cancelled paste managed to create.
        let Some(OpRecord::Copy { created }) = report.record.clone() else {
            panic!("a cancelled paste still journals what it did: {report:?}");
        };
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].root, dest.join("a.txt"));
        super::super::journal::undo_record(&report.record.unwrap(), &TaskCtx::detached()).unwrap();
        assert!(!exists(&dest.join("a.txt")));
    }

    #[test]
    fn unique_name_skips_taken_and_claimed_names() {
        let t = TempTree::new("unique");
        let dir = t.dir("d");
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let claimed = vec![dir.join("a_1.txt")];
        assert_eq!(
            unique_name(&dir, OsStr::new("a.txt"), &claimed).unwrap(),
            dir.join("a_2.txt")
        );
        assert_eq!(
            unique_name(&dir, OsStr::new("free.txt"), &[]).unwrap(),
            dir.join("free.txt")
        );
    }

    #[test]
    fn a_clipboard_can_be_cleared() {
        let mut clip = Clipboard::yank([PathBuf::from("/tmp/a")]);
        assert_eq!(clip.len(), 1);
        assert_eq!(clip.mode, PasteMode::Copy);
        clip.clear();
        assert!(clip.is_empty());
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// One `b` puts the batch in, the next takes the same batch out — and a
    /// path offered twice is held once.
    #[test]
    fn toggling_is_a_toggle_over_the_whole_batch() {
        let mut clip = Clipboard::default();
        let batch = paths(&["/a", "/b"]);
        assert_eq!(
            clip.toggle(&batch),
            Toggled {
                added: 2,
                removed: 0
            }
        );
        assert_eq!(clip.len(), 2);

        // Already in, all of it: the same press takes it back out.
        assert_eq!(
            clip.toggle(&batch),
            Toggled {
                added: 0,
                removed: 2
            }
        );
        assert!(clip.is_empty());

        // A mixed batch goes *in* — the press has to mean one thing.
        clip.toggle(&paths(&["/a"]));
        assert_eq!(
            clip.toggle(&paths(&["/a", "/c", "/c"])),
            Toggled {
                added: 1,
                removed: 0
            }
        );
        assert_eq!(clip.paths, paths(&["/a", "/c"]));
    }

    /// Insertion order, because that is the one thing a person tracks about a
    /// pile they made by hand; and the tray's `×` takes out exactly one.
    #[test]
    fn the_order_is_the_order_things_were_added() {
        let mut clip = Clipboard::default();
        clip.toggle(&paths(&["/z"]));
        clip.toggle(&paths(&["/a"]));
        clip.toggle(&paths(&["/m"]));
        assert_eq!(clip.paths, paths(&["/z", "/a", "/m"]));
        assert_eq!(clip.remove(1), Some(PathBuf::from("/a")));
        assert_eq!(clip.paths, paths(&["/z", "/m"]));
        assert_eq!(clip.remove(9), None);
    }

    /// An empty press does nothing, and says nothing.
    #[test]
    fn toggling_nothing_is_a_no_op() {
        let mut clip = Clipboard::cut([PathBuf::from("/a")]);
        assert_eq!(clip.toggle(&[]), Toggled::default());
        assert_eq!(clip.paths, paths(&["/a"]));
        assert_eq!(clip.mode, PasteMode::Cut);
    }

    /// Adding to a cut is adding to the cut; a pile started from nothing is a
    /// copy, whatever the clipboard was the last time it held anything.
    #[test]
    fn toggling_keeps_the_verb_unless_there_was_nothing_to_keep() {
        let mut clip = Clipboard::cut([PathBuf::from("/a")]);
        clip.toggle(&paths(&["/b"]));
        assert_eq!(
            clip.mode,
            PasteMode::Cut,
            "a `b` turned the cut into a copy"
        );
        assert_eq!(clip.paths, paths(&["/a", "/b"]));

        // Taken back out down to nothing, and started again: a copy.
        clip.toggle(&paths(&["/a", "/b"]));
        assert!(clip.is_empty());
        clip.toggle(&paths(&["/c"]));
        assert_eq!(clip.mode, PasteMode::Copy);

        // `X` leaves the old verb behind; the next pile does not inherit it.
        let mut clip = Clipboard::cut([PathBuf::from("/a")]);
        clip.clear();
        clip.toggle(&paths(&["/b"]));
        assert_eq!(clip.mode, PasteMode::Copy);
    }

    /// The same file, spelled two ways, is one file: the paths are held
    /// normalised, exactly as `y` and `x` hold them, which is what lets a row
    /// in another directory find its mark by path.
    #[test]
    fn toggling_holds_paths_the_way_a_yank_does() {
        let mut clip = Clipboard::default();
        clip.toggle(&paths(&["/d/./e/../f.txt"]));
        assert_eq!(clip.paths, paths(&["/d/f.txt"]));
        assert!(clip.contains(Path::new("/d/f.txt")));
        assert!(clip.contains(Path::new("/d/e/../f.txt")));
        assert!(!clip.contains(Path::new("/d/g.txt")));
        assert_eq!(
            clip.toggle(&paths(&["/d/f.txt"])),
            Toggled {
                added: 0,
                removed: 1
            }
        );
        assert_eq!(
            Clipboard::yank(paths(&["/d/./f.txt"])).paths,
            paths(&["/d/f.txt"])
        );
    }

    /// One directory is an ordinary yank; two is a pile somebody built.
    #[test]
    fn a_clipboard_spans_directories_when_its_parents_differ() {
        assert!(!Clipboard::default().spans_directories());
        assert!(!Clipboard::yank(paths(&["/d/a", "/d/b", "/d/c"])).spans_directories());
        assert!(Clipboard::yank(paths(&["/d/a", "/e/b"])).spans_directories());
        // A folder and a file inside it are in two directories.
        assert!(Clipboard::yank(paths(&["/d/sub", "/d/sub/x"])).spans_directories());
    }
}
