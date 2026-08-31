//! The op journal: what `u` undoes, and the reason it is safe to press.
//!
//! Every mutation records its inverse (PLAN §5). The interesting half is not
//! the inverse — it is the refusal: between the operation and the undo, the
//! world may have changed, and an undo that plows through a change is worse
//! than no undo at all. So every entry carries a [`Fingerprint`] of what it
//! created, every inverse checks it *before touching anything*, and a mismatch
//! is an error the user reads rather than a deletion they discover.
//!
//! Two things are deliberately absent:
//!
//! - **Permanent delete is not journalled.** There is nothing to record: the
//!   bytes are gone. It is the one irreversible operation, which is why it is
//!   the one with a confirm dialog (PLAN §5).
//! - **Redo.** Not required for v1 (PLAN §5). The stack only pops.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::tasks::TaskCtx;
use crate::{DfError, Result};

use super::trash::TrashedItem;
use super::{exists, normalize};

/// How many operations `u` can walk back.
///
/// 64 is two or three sessions' worth of real editing — far more than the
/// half-dozen anyone remembers doing — while keeping the journal a few
/// kilobytes of paths rather than an unbounded log of every rename since the
/// program started. The bound matters because entries hold paths into the
/// trash: an unbounded journal is also an unbounded promise that those trash
/// files still exist.
pub const JOURNAL_DEPTH: usize = 64;

/// What kind of thing a path was when the operation finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Enough of a path's state to notice that the world moved on.
///
/// Not a checksum: hashing every byte of everything an operation touched would
/// make `u` cost as much as the operation did. Kind, size and mtime catch
/// editing, replacing and truncating — the ways a file actually changes between
/// an operation and the undo of it seconds later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub kind: FileKind,
    pub len: u64,
    pub mtime: Option<SystemTime>,
    /// For directories: how many entries it had. A directory's own mtime moves
    /// whenever anything inside it does, so comparing it would refuse almost
    /// every undo; the child count is the cheap middle ground that still
    /// notices "somebody put new work in here".
    pub entries: Option<u64>,
}

impl Fingerprint {
    pub fn of(path: &Path) -> Result<Fingerprint> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| DfError::io(path, e))?;
        let kind = if meta.is_symlink() {
            FileKind::Symlink
        } else if meta.is_dir() {
            FileKind::Dir
        } else if meta.is_file() {
            FileKind::File
        } else {
            FileKind::Other
        };
        let entries = if kind == FileKind::Dir {
            Some(
                std::fs::read_dir(path)
                    .map_err(|e| DfError::io(path, e))?
                    .count() as u64,
            )
        } else {
            None
        };
        Ok(Fingerprint {
            kind,
            len: meta.len(),
            mtime: meta.modified().ok(),
            entries,
        })
    }

    /// Is `path` still what it was? The error text says what changed, because
    /// "cannot undo" without a reason is a dead end.
    pub fn verify(&self, path: &Path) -> Result<()> {
        let now = Fingerprint::of(path).map_err(|_| {
            DfError::Op(format!(
                "cannot undo: {} is no longer there",
                path.display()
            ))
        })?;
        if now.kind != self.kind {
            return Err(DfError::Op(format!(
                "cannot undo: {} is not the same kind of file any more",
                path.display()
            )));
        }
        match self.kind {
            FileKind::Dir => {
                if now.entries != self.entries {
                    return Err(DfError::Op(format!(
                        "cannot undo: {} has different contents now",
                        path.display()
                    )));
                }
            }
            FileKind::File => {
                if now.len != self.len {
                    return Err(DfError::Op(format!(
                        "cannot undo: {} has changed size",
                        path.display()
                    )));
                }
                if let (Some(a), Some(b)) = (self.mtime, now.mtime) {
                    if a != b {
                        return Err(DfError::Op(format!(
                            "cannot undo: {} has been modified since",
                            path.display()
                        )));
                    }
                }
            }
            FileKind::Symlink | FileKind::Other => {}
        }
        Ok(())
    }
}

/// One leg of a move, with the fingerprint of where it landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedPath {
    pub from: PathBuf,
    pub to: PathBuf,
    pub fingerprint: Fingerprint,
}

impl MovedPath {
    pub fn record(from: &Path, to: &Path) -> Result<MovedPath> {
        Ok(MovedPath {
            from: normalize(from),
            to: normalize(to),
            fingerprint: Fingerprint::of(to)?,
        })
    }
}

/// A completed operation, and everything its inverse needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpRecord {
    /// A cut-and-paste of one or more paths. Inverse: move each back.
    Move { moves: Vec<MovedPath> },
    /// `r` / `R`. Inverse: rename back. Kept apart from `Move` only so the
    /// toast can say "renamed" — which is what the user did.
    Rename { moved: MovedPath },
    /// A yank-and-paste. Inverse: delete what was created, and nothing else.
    Copy { created: Vec<(PathBuf, Fingerprint)> },
    /// `d`. Inverse: restore each item from the trash it went into.
    Trash { items: Vec<TrashedItem> },
    /// `a`. Inverse: remove it, if it is still the empty thing that was made.
    Create {
        path: PathBuf,
        is_dir: bool,
        fingerprint: Fingerprint,
        /// Parent directories the create had to make. Peeled off after the
        /// file, deepest first, and only while they are still empty.
        created_parents: Vec<PathBuf>,
    },
    /// `-` / `_` / `Ctrl+-`. Inverse: unlink.
    Link {
        link: PathBuf,
        /// The link text, for a symlink. `None` for a hard link.
        target: Option<PathBuf>,
        fingerprint: Fingerprint,
    },
}

impl OpRecord {
    /// One line for the `u` toast, in the past tense of what would happen.
    pub fn describe(&self) -> String {
        fn plural(n: usize, one: &str, many: &str) -> String {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{n} {many}")
            }
        }
        match self {
            OpRecord::Move { moves } => format!("moved {}", plural(moves.len(), "item", "items")),
            OpRecord::Rename { moved } => format!(
                "renamed {}",
                moved
                    .to
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            OpRecord::Copy { created } => {
                format!("copied {}", plural(created.len(), "item", "items"))
            }
            OpRecord::Trash { items } => {
                format!("trashed {}", plural(items.len(), "item", "items"))
            }
            OpRecord::Create { path, is_dir, .. } => format!(
                "created {} {}",
                if *is_dir { "folder" } else { "file" },
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            OpRecord::Link { link, .. } => format!(
                "linked {}",
                link.file_name().unwrap_or_default().to_string_lossy()
            ),
        }
    }
}

/// What an undo did, for the toast (PLAN §5: every op lands with an 8 s undo
/// toast, and the undo itself says what it took back).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoReport {
    pub description: String,
    /// Paths that exist again, or that no longer do — whatever the UI should
    /// move the cursor to.
    pub touched: Vec<PathBuf>,
}

/// The bounded stack of inverses.
#[derive(Debug, Clone)]
pub struct Journal {
    entries: VecDeque<OpRecord>,
    depth: usize,
}

impl Default for Journal {
    fn default() -> Journal {
        Journal::new(JOURNAL_DEPTH)
    }
}

impl Journal {
    pub fn new(depth: usize) -> Journal {
        Journal {
            entries: VecDeque::new(),
            // A depth of 0 would silently make `u` do nothing; one step of undo
            // is the minimum that is not a lie.
            depth: depth.max(1),
        }
    }

    /// Push a completed operation. The oldest entry falls off the bottom once
    /// the journal is full.
    pub fn record(&mut self, record: OpRecord) {
        self.entries.push_back(record);
        while self.entries.len() > self.depth {
            self.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    /// What `u` would take back next, without taking it back.
    pub fn peek(&self) -> Option<&OpRecord> {
        self.entries.back()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Undo the most recent operation.
    ///
    /// The entry stays on the stack if the undo fails, so the user can fix
    /// whatever is in the way (close the file, move the newer copy aside) and
    /// press `u` again.
    pub fn undo(&mut self, ctx: &TaskCtx) -> Result<UndoReport> {
        let Some(entry) = self.entries.back().cloned() else {
            return Err(DfError::Op("nothing to undo".to_string()));
        };
        let report = undo_record(&entry, ctx)?;
        self.entries.pop_back();
        Ok(report)
    }
}

/// Run one inverse. Public so a caller can undo a record it is holding without
/// a journal — the conflict dialog's "put that back" path.
pub fn undo_record(record: &OpRecord, ctx: &TaskCtx) -> Result<UndoReport> {
    match record {
        OpRecord::Move { moves } => undo_moves(moves, ctx, "Moved"),
        OpRecord::Rename { moved } => {
            undo_moves(std::slice::from_ref(moved), ctx, "Renamed")
        }
        OpRecord::Copy { created } => undo_copy(created, ctx),
        OpRecord::Trash { items } => undo_trash(items, ctx),
        OpRecord::Create {
            path,
            is_dir,
            fingerprint,
            created_parents,
        } => undo_create(path, *is_dir, fingerprint, created_parents),
        OpRecord::Link {
            link,
            target,
            fingerprint,
        } => undo_link(link, target.as_deref(), fingerprint),
    }
}

/// Move everything back where it came from.
///
/// Checked in full first: a five-file paste that can only put three back should
/// put none back, rather than leaving the user with a half-undone state they
/// now have to reason about.
fn undo_moves(moves: &[MovedPath], ctx: &TaskCtx, verb: &str) -> Result<UndoReport> {
    for m in moves {
        m.fingerprint.verify(&m.to)?;
        if exists(&m.from) {
            return Err(DfError::Op(format!(
                "cannot undo: {} exists again",
                m.from.display()
            )));
        }
        let Some(parent) = m.from.parent() else {
            return Err(DfError::Op(format!("{} has no parent", m.from.display())));
        };
        if !exists(parent) {
            return Err(DfError::Op(format!(
                "cannot undo: {} no longer exists",
                parent.display()
            )));
        }
    }

    let mut touched = Vec::new();
    for m in moves {
        super::copy::move_path(&m.to, &m.from, ctx, false)?;
        touched.push(m.from.clone());
    }
    let what = if moves.len() == 1 {
        moves[0].from.file_name().unwrap_or_default().to_string_lossy().to_string()
    } else {
        format!("{} items", moves.len())
    };
    Ok(UndoReport {
        description: format!("{verb} {what} back"),
        touched,
    })
}

/// Delete exactly what the copy created, and only if it is untouched.
fn undo_copy(created: &[(PathBuf, Fingerprint)], ctx: &TaskCtx) -> Result<UndoReport> {
    for (path, fp) in created {
        fp.verify(path)?;
    }
    let mut touched = Vec::new();
    for (path, _fp) in created {
        super::delete::remove_tree(path, ctx)?;
        touched.push(path.clone());
    }
    Ok(UndoReport {
        description: if created.len() == 1 {
            format!(
                "Removed the copy {}",
                created[0].0.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            format!("Removed {} copies", created.len())
        },
        touched,
    })
}

fn undo_trash(items: &[TrashedItem], ctx: &TaskCtx) -> Result<UndoReport> {
    // `restore` already refuses to overwrite; check every item first so a
    // multi-file `d` is put back all at once or not at all.
    for item in items {
        if !exists(&item.files_path()) {
            return Err(DfError::Op(format!(
                "cannot undo: {} is no longer in the trash",
                item.original.display()
            )));
        }
        if exists(&item.original) {
            return Err(DfError::Op(format!(
                "cannot undo: {} exists again",
                item.original.display()
            )));
        }
    }
    let mut touched = Vec::new();
    for item in items {
        touched.push(super::trash::restore(item, ctx)?);
    }
    Ok(UndoReport {
        description: if items.len() == 1 {
            format!(
                "Restored {}",
                items[0].original.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            format!("Restored {} items from the trash", items.len())
        },
        touched,
    })
}

fn undo_create(
    path: &Path,
    is_dir: bool,
    fingerprint: &Fingerprint,
    created_parents: &[PathBuf],
) -> Result<UndoReport> {
    fingerprint.verify(path)?;
    if is_dir {
        // `remove_dir` fails on a non-empty directory, which is exactly the
        // check that is wanted — and it is atomic, unlike looking first.
        std::fs::remove_dir(path).map_err(|e| {
            DfError::Op(format!(
                "cannot undo: {} is not empty any more ({e})",
                path.display()
            ))
        })?;
    } else {
        std::fs::remove_file(path).map_err(|e| DfError::io(path, e))?;
    }
    // Peel the parents this create made, deepest first, stopping at the first
    // one somebody else has put something in.
    for dir in created_parents.iter().rev() {
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
    }
    Ok(UndoReport {
        description: format!(
            "Removed {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
        touched: vec![path.to_path_buf()],
    })
}

fn undo_link(link: &Path, target: Option<&Path>, fingerprint: &Fingerprint) -> Result<UndoReport> {
    fingerprint.verify(link)?;
    if let Some(target) = target {
        let now = std::fs::read_link(link).map_err(|e| DfError::io(link, e))?;
        if now != target {
            return Err(DfError::Op(format!(
                "cannot undo: {} points somewhere else now",
                link.display()
            )));
        }
    }
    std::fs::remove_file(link).map_err(|e| DfError::io(link, e))?;
    Ok(UndoReport {
        description: format!(
            "Removed the link {}",
            link.file_name().unwrap_or_default().to_string_lossy()
        ),
        touched: vec![link.to_path_buf()],
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;
    use crate::ops::link::LinkKind;
    use crate::ops::{copy, create, link, trash};

    fn ctx() -> TaskCtx {
        TaskCtx::detached()
    }

    #[test]
    fn undo_of_a_move_puts_it_back() {
        let t = TempTree::new("j-move");
        let from = t.file("a/x.txt", b"body");
        t.dir("b");
        let to = t.join("b/x.txt");
        copy::move_path(&from, &to, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Move {
            moves: vec![MovedPath::record(&from, &to).unwrap()],
        });

        let report = j.undo(&ctx()).unwrap();
        assert!(report.description.contains("back"), "{report:?}");
        assert_eq!(std::fs::read(&from).unwrap(), b"body");
        assert!(!super::exists(&to));
        assert!(j.is_empty());
    }

    #[test]
    fn undo_of_a_multi_move_is_all_or_nothing() {
        let t = TempTree::new("j-move-many");
        let a = t.file("src/a.txt", b"a");
        let b = t.file("src/b.txt", b"b");
        t.dir("dst");
        let a2 = t.join("dst/a.txt");
        let b2 = t.join("dst/b.txt");
        copy::move_path(&a, &a2, &ctx(), false).unwrap();
        copy::move_path(&b, &b2, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Move {
            moves: vec![
                MovedPath::record(&a, &a2).unwrap(),
                MovedPath::record(&b, &b2).unwrap(),
            ],
        });

        // Somebody made a new a.txt where the first one came from.
        std::fs::write(&a, b"newer work").unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("exists again"), "{err}");
        assert_eq!(
            std::fs::read(&b2).unwrap(),
            b"b",
            "nothing was moved back at all"
        );
        assert_eq!(j.len(), 1, "the entry survives a refused undo");

        // Clear the way and it works.
        std::fs::remove_file(&a).unwrap();
        j.undo(&ctx()).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"a");
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
    }

    #[test]
    fn undo_of_a_rename_renames_back() {
        let t = TempTree::new("j-rename");
        let from = t.file("old.txt", b"x");
        let to = t.join("new.txt");
        create::rename(&from, &to, false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Rename {
            moved: MovedPath::record(&from, &to).unwrap(),
        });
        let report = j.undo(&ctx()).unwrap();
        assert!(report.description.starts_with("Renamed"), "{report:?}");
        assert!(from.is_file());
        assert!(!super::exists(&to));
    }

    #[test]
    fn undo_of_a_copy_deletes_the_copies_only() {
        let t = TempTree::new("j-copy");
        let src = t.file("src/a.txt", b"data");
        let dst = t.join("dst-a.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![(dst.clone(), Fingerprint::of(&dst).unwrap())],
        });
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&dst));
        assert_eq!(std::fs::read(&src).unwrap(), b"data", "the original stays");
    }

    #[test]
    fn undo_of_a_copy_refuses_a_modified_copy() {
        let t = TempTree::new("j-copy-changed");
        let src = t.file("a.txt", b"data");
        let dst = t.join("b.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![(dst.clone(), Fingerprint::of(&dst).unwrap())],
        });
        std::fs::write(&dst, b"the user edited this").unwrap();

        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert!(dst.is_file(), "newer work is never destroyed by undo");
    }

    #[test]
    fn undo_of_a_copied_directory_refuses_new_contents() {
        let t = TempTree::new("j-copy-dir");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"a").unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![(dst.clone(), Fingerprint::of(&dst).unwrap())],
        });
        std::fs::write(dst.join("new-work"), b"mine").unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("different contents"), "{err}");
        assert!(dst.join("new-work").is_file());
    }

    #[test]
    fn undo_of_a_trash_restores_it() {
        let t = TempTree::new("j-trash");
        let bin = trash::Trash::at(t.join("Trash"));
        let file = t.file("work/notes.txt", b"contents");
        let item = bin.trash(&file, &ctx()).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Trash {
            items: vec![item.clone()],
        });
        let report = j.undo(&ctx()).unwrap();
        assert!(report.description.starts_with("Restored"), "{report:?}");
        assert_eq!(std::fs::read(&file).unwrap(), b"contents");
    }

    #[test]
    fn undo_of_a_trash_refuses_when_the_name_came_back() {
        let t = TempTree::new("j-trash-taken");
        let bin = trash::Trash::at(t.join("Trash"));
        let file = t.file("work/notes.txt", b"old");
        let item = bin.trash(&file, &ctx()).unwrap();
        std::fs::write(&file, b"newer").unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Trash { items: vec![item] });
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("exists again"), "{err}");
        assert_eq!(std::fs::read(&file).unwrap(), b"newer");
    }

    #[test]
    fn undo_of_a_create_removes_it_with_its_parents() {
        let t = TempTree::new("j-create");
        let made = create::create(&t.join("a/b/notes.md")).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Create {
            path: made.path.clone(),
            is_dir: made.is_dir,
            fingerprint: Fingerprint::of(&made.path).unwrap(),
            created_parents: made.created_parents.clone(),
        });
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&made.path));
        assert!(!super::exists(&t.join("a")), "the parents go too");
    }

    #[test]
    fn undo_of_a_create_refuses_a_file_that_has_been_written_to() {
        let t = TempTree::new("j-create-written");
        let made = create::create(&t.join("notes.md")).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Create {
            path: made.path.clone(),
            is_dir: false,
            fingerprint: Fingerprint::of(&made.path).unwrap(),
            created_parents: Vec::new(),
        });
        std::fs::write(&made.path, b"the user typed something").unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert!(made.path.is_file());
    }

    #[test]
    fn undo_of_a_created_directory_refuses_when_it_is_no_longer_empty() {
        let t = TempTree::new("j-create-dir");
        let made = create::create(&t.join("folder/")).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Create {
            path: made.path.clone(),
            is_dir: true,
            fingerprint: Fingerprint::of(&made.path).unwrap(),
            created_parents: Vec::new(),
        });
        std::fs::write(made.path.join("something"), b"x").unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert!(made.path.is_dir());
    }

    #[test]
    fn undo_of_a_symlink_unlinks_it() {
        let t = TempTree::new("j-link");
        let target = t.file("target.txt", b"x");
        let l = t.join("link");
        let text = link::symlink(&target, &l, LinkKind::Absolute).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Link {
            link: l.clone(),
            target: Some(text),
            fingerprint: Fingerprint::of(&l).unwrap(),
        });
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&l));
        assert!(target.is_file(), "the target is untouched");
    }

    #[test]
    fn undo_of_a_symlink_refuses_when_it_points_elsewhere_now() {
        let t = TempTree::new("j-link-changed");
        let target = t.file("target.txt", b"x");
        let l = t.join("link");
        let text = link::symlink(&target, &l, LinkKind::Absolute).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Link {
            link: l.clone(),
            target: Some(text),
            fingerprint: Fingerprint::of(&l).unwrap(),
        });

        std::fs::remove_file(&l).unwrap();
        std::os::unix::fs::symlink("/somewhere/else", &l).unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("points somewhere else"), "{err}");
        assert!(super::exists(&l));
    }

    #[test]
    fn undo_of_a_hardlink_unlinks_it() {
        let t = TempTree::new("j-hardlink");
        let target = t.file("target.txt", b"x");
        let l = t.join("hard");
        link::hardlink(&target, &l).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Link {
            link: l.clone(),
            target: None,
            fingerprint: Fingerprint::of(&l).unwrap(),
        });
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&l));
        assert_eq!(std::fs::read(&target).unwrap(), b"x");
    }

    #[test]
    fn the_journal_is_bounded() {
        let mut j = Journal::new(3);
        for i in 0..10 {
            j.record(OpRecord::Create {
                path: PathBuf::from(format!("/tmp/{i}")),
                is_dir: false,
                fingerprint: Fingerprint {
                    kind: FileKind::File,
                    len: 0,
                    mtime: None,
                    entries: None,
                },
                created_parents: Vec::new(),
            });
        }
        assert_eq!(j.len(), 3);
        // The three newest survived; the oldest fell off the bottom.
        match j.peek() {
            Some(OpRecord::Create { path, .. }) => assert_eq!(path, Path::new("/tmp/9")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_depth_of_zero_still_keeps_one() {
        let mut j = Journal::new(0);
        assert_eq!(j.depth(), 1);
        j.record(OpRecord::Copy { created: vec![] });
        assert_eq!(j.len(), 1);
    }

    #[test]
    fn undo_with_nothing_recorded_says_so() {
        let mut j = Journal::default();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("nothing to undo"), "{err}");
    }

    #[test]
    fn default_depth_is_the_constant() {
        assert_eq!(Journal::default().depth(), JOURNAL_DEPTH);
    }

    #[test]
    fn records_describe_themselves() {
        let t = TempTree::new("j-describe");
        let p = t.file("a.txt", b"x");
        let fp = Fingerprint::of(&p).unwrap();
        assert_eq!(
            OpRecord::Copy {
                created: vec![(p.clone(), fp.clone())]
            }
            .describe(),
            "copied 1 item"
        );
        assert_eq!(
            OpRecord::Move {
                moves: vec![
                    MovedPath {
                        from: p.clone(),
                        to: p.clone(),
                        fingerprint: fp.clone()
                    };
                    3
                ]
            }
            .describe(),
            "moved 3 items"
        );
        assert!(OpRecord::Create {
            path: p.clone(),
            is_dir: true,
            fingerprint: fp,
            created_parents: vec![]
        }
        .describe()
        .contains("folder"));
    }

    #[test]
    fn fingerprint_notices_a_kind_change() {
        let t = TempTree::new("fp-kind");
        let p = t.file("a", b"x");
        let fp = Fingerprint::of(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        let err = fp.verify(&p).unwrap_err();
        assert!(err.to_string().contains("same kind"), "{err}");
    }

    #[test]
    fn fingerprint_notices_a_missing_path() {
        let t = TempTree::new("fp-missing");
        let p = t.file("a", b"x");
        let fp = Fingerprint::of(&p).unwrap();
        std::fs::remove_file(&p).unwrap();
        let err = fp.verify(&p).unwrap_err();
        assert!(err.to_string().contains("no longer there"), "{err}");
    }
}
