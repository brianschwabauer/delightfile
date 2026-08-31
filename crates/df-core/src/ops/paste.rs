//! Yank, cut and paste — `y`, `x`, `p`, `P`.
//!
//! The clipboard is a set of paths and a verb, nothing more. The interesting
//! part is [`plan_paste`]: given a clipboard and a destination, it works out
//! what each item would become, and hands back the collisions **as data** for
//! the UI to resolve with the side-by-side dialog (PLAN §5). It never decides
//! for the user. `P` — paste with force — is the one exception, and even it
//! cannot talk the safety rails out of anything.
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

use std::path::{Path, PathBuf};

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::journal::{Fingerprint, MovedPath, OpRecord};
use super::trash::{suffixed, MAX_TRASH_COLLISIONS};
use super::{exists, file_name, is_ancestor_resolved, is_real_dir, normalize, same_file};

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
    pub paths: Vec<PathBuf>,
}

impl Clipboard {
    /// `y`: copy on paste.
    pub fn yank(paths: impl IntoIterator<Item = PathBuf>) -> Clipboard {
        Clipboard {
            mode: PasteMode::Copy,
            paths: paths.into_iter().map(|p| normalize(&p)).collect(),
        }
    }

    /// `x`: move on paste.
    pub fn cut(paths: impl IntoIterator<Item = PathBuf>) -> Clipboard {
        Clipboard {
            mode: PasteMode::Cut,
            paths: paths.into_iter().map(|p| normalize(&p)).collect(),
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
    /// Keep both, under this name.
    Rename(PathBuf),
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
    pub fn resolve(&mut self, src: &Path, answer: &Resolution) {
        let Some(index) = self.conflicts.iter().position(|c| c.src == src) else {
            return;
        };
        let conflict = self.conflicts.remove(index);
        self.apply_one(conflict, answer);
    }

    /// The dialog's "apply to all" — the same answer for every remaining
    /// conflict.
    pub fn resolve_all(&mut self, answer: &Resolution) {
        for conflict in std::mem::take(&mut self.conflicts) {
            // A single explicit rename cannot be applied to many files without
            // colliding, so "rename all" means "keep both, auto-named" — which
            // is the suggested name each conflict already carries.
            let answer = match answer {
                Resolution::Rename(_) => Resolution::Rename(conflict.suggested.clone()),
                other => other.clone(),
            };
            self.apply_one(conflict, &answer);
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
                // A bare name is taken as a name in the destination; a path is
                // taken as given, so the dialog can offer either.
                let dst = if name.parent() == Some(Path::new("")) {
                    self.dest_dir.join(name)
                } else {
                    name.clone()
                };
                self.ready.push(PasteItem {
                    src: conflict.src,
                    dst,
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
    let dest_dir = normalize(dest_dir);
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
        let src = normalize(src);
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
pub fn unique_name(
    dir: &Path,
    name: &std::ffi::OsStr,
    claimed: &[PathBuf],
) -> Result<PathBuf> {
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

    let mut created: Vec<(PathBuf, Fingerprint)> = Vec::new();
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
            PasteMode::Copy => super::copy::copy_tree(&item.src, &item.dst, ctx, item.overwrite)
                .map(|_stats| ()),
            PasteMode::Cut => super::copy::move_path(&item.src, &item.dst, ctx, item.overwrite),
        };
        match outcome {
            Ok(()) => match plan.mode {
                PasteMode::Copy => {
                    match Fingerprint::of(&item.dst) {
                        Ok(fp) if fresh => created.push((item.dst.clone(), fp)),
                        Ok(_) => log::info!(
                            "no undo record for {}: it was already there",
                            item.dst.display()
                        ),
                        // The copy landed but cannot be fingerprinted; leaving
                        // it out of the journal is the safe half — undo will
                        // not delete something it cannot verify.
                        Err(e) => log::warn!("no undo record for {}: {e}", item.dst.display()),
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
        plan.resolve(&normalize(&src), &Resolution::Overwrite);
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
        plan.resolve(&normalize(&src), &Resolution::Skip);
        assert!(plan.ready.is_empty());
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"old");

        let mut plan = plan_paste(&Clipboard::yank([src.clone()]), &dest, false).unwrap();
        let suggested = plan.conflicts[0].suggested.clone();
        plan.resolve(&normalize(&src), &Resolution::Rename(suggested.clone()));
        execute(&plan, &ctx()).unwrap();
        assert_eq!(std::fs::read(&suggested).unwrap(), b"new");
        assert_eq!(std::fs::read(dest.join("notes.txt")).unwrap(), b"old");
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
        let mut plan = plan_paste(&Clipboard::yank([doomed.clone(), good.clone()]), &dest, false)
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
}
