//! The op journal: what `u` undoes, and the reason it is safe to press.
//!
//! Every mutation records its inverse (PLAN §5). The interesting half is not
//! the inverse — it is the refusal: between the operation and the undo, the
//! world may have changed, and an undo that plows through a change is worse
//! than no undo at all. So every entry carries a [`Fingerprint`] of what it
//! created, every inverse checks it *before touching anything*, and a mismatch
//! is an error the user reads rather than a deletion they discover.
//!
//! A copy carries a whole [`CopyManifest`] instead of one fingerprint, because
//! it is the one inverse that *deletes*: a fingerprint describes a single path,
//! and a directory's does not move when a file is added two levels below it.
//! The manifest names every path the copy created, the undo verifies all of
//! them — and that no recorded directory holds anything else — before removing
//! any, and then removes exactly those paths, children first. There is no
//! recursive delete anywhere in an undo.
//!
//! Two things are deliberately absent:
//!
//! - **Permanent delete is not journalled.** There is nothing to record: the
//!   bytes are gone. It is the one irreversible operation, which is why it is
//!   the one with a confirm dialog (PLAN §5).
//! - **Redo.** Not required for v1 (PLAN §5). The stack only pops.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::tasks::TaskCtx;
use crate::text::grouped;
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

/// How many paths one copy may record before it stops being undoable.
///
/// A manifest costs roughly the length of each relative path plus ~40 bytes of
/// fingerprint, so 50 000 entries is a few megabytes — and the journal holds up
/// to [`JOURNAL_DEPTH`] of them. Past this the copy is simply not journalled:
/// `u` says there is nothing to undo rather than quietly holding a hundred
/// megabytes of paths, or — far worse — deleting a 200 000-file tree on the
/// strength of a fingerprint that could not describe it. Undoing a copy that
/// big is a recursive delete the user should ask for explicitly.
pub const MAX_MANIFEST_ENTRIES: usize = 50_000;

/// Every path one copy created, and the state each was in when it finished.
///
/// The reason this exists rather than a single [`Fingerprint`] on the top-level
/// destination: a fingerprint describes *one* path. For a copied tree the top
/// directory's child count does not move when work is added two levels down, so
/// an undo that checked only the top would walk in and `remove_tree` the lot —
/// "press u, lose today's work". A manifest makes the undo check every path it
/// is about to delete, and delete nothing else.
///
/// ## Memory
///
/// Paths are stored *relative to [`root`](CopyManifest::root)*, which is the one
/// absolute path in here. A 40 000-file tree under `~/Work/very/long/prefix`
/// costs the relative names once instead of the prefix 40 000 times. The
/// remaining cost is real and bounded on purpose by [`MAX_MANIFEST_ENTRIES`]:
/// the alternative — recording nothing and trusting one fingerprint — is the
/// bug this type replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyManifest {
    /// The destination the copy created.
    pub root: PathBuf,
    /// Every created path relative to `root`, depth-first, parents before
    /// children. The root itself is the first entry, with an empty relative
    /// path.
    entries: Vec<(PathBuf, Fingerprint)>,
}

impl CopyManifest {
    /// Record everything that now lives at `root`.
    ///
    /// Called immediately after a copy, on a destination the copy *created* —
    /// so what is on disk is exactly what it made. That is also what makes a
    /// partial copy honest: [`super::copy::copy_tree`] removes a destination it
    /// created but could not finish, and a paste that is cancelled between
    /// items never reaches this call for the item it did not start, so each
    /// manifest describes one item that actually landed, whole.
    ///
    /// Errors if the tree is larger than [`MAX_MANIFEST_ENTRIES`], which the
    /// caller turns into "this copy is not undoable" rather than into a failure
    /// of the copy.
    pub fn of_tree(root: &Path) -> Result<CopyManifest> {
        CopyManifest::of_tree_capped(root, MAX_MANIFEST_ENTRIES)
    }

    /// [`of_tree`](CopyManifest::of_tree) with the bound spelled out, so the
    /// refusal is testable without making fifty thousand files.
    pub(crate) fn of_tree_capped(root: &Path, cap: usize) -> Result<CopyManifest> {
        let root = normalize(root);
        let mut entries = Vec::new();
        walk(&root, PathBuf::new(), &mut entries, cap)?;
        Ok(CopyManifest { root, entries })
    }

    /// How many paths the copy created.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every path this manifest covers, absolute.
    pub fn paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.entries.iter().map(|(rel, _)| self.path_of(rel))
    }

    fn path_of(&self, rel: &Path) -> PathBuf {
        if rel.as_os_str().is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }

    /// Is the world still exactly as the copy left it?
    ///
    /// Two questions, both of which have to be "yes" before anything is
    /// deleted:
    ///
    /// 1. every recorded path is still there, still the same kind, still the
    ///    same size and mtime;
    /// 2. no directory the copy created holds anything the manifest does not
    ///    account for — which is the nested case a single fingerprint misses.
    ///
    /// The error names the path that changed, because "cannot undo" without a
    /// reason is a dead end.
    pub fn verify(&self) -> Result<()> {
        // The names each recorded directory is allowed to contain.
        let mut expected: HashMap<&Path, HashSet<&OsStr>> = HashMap::new();
        for (rel, _) in &self.entries {
            if let (Some(parent), Some(name)) = (rel.parent(), rel.file_name()) {
                expected.entry(parent).or_default().insert(name);
            }
        }
        let none: HashSet<&OsStr> = HashSet::new();

        for (rel, fp) in &self.entries {
            let path = self.path_of(rel);
            fp.verify(&path)?;
            if fp.kind != FileKind::Dir {
                continue;
            }
            let known = expected.get(rel.as_path()).unwrap_or(&none);
            for entry in std::fs::read_dir(&path).map_err(|e| DfError::io(&path, e))? {
                let entry = entry.map_err(|e| DfError::io(&path, e))?;
                if !known.contains(entry.file_name().as_os_str()) {
                    return Err(DfError::Op(format!(
                        "cannot undo: {} was added after the copy",
                        entry.path().display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Delete exactly the manifested paths, children before parents.
    ///
    /// Never a blanket `remove_tree`: `remove_dir` on a directory that is not
    /// empty fails, which is one last rail under [`CopyManifest::verify`] —
    /// anything that appeared between the check and the delete keeps its
    /// directory, and the error names it.
    pub fn remove(&self, ctx: &TaskCtx) -> Result<()> {
        // The entries are parents-before-children, so reversed is every child
        // ahead of the directory holding it.
        for (rel, fp) in self.entries.iter().rev() {
            ctx.checkpoint()?;
            let path = self.path_of(rel);
            let outcome = if fp.kind == FileKind::Dir {
                std::fs::remove_dir(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match outcome {
                Ok(()) => ctx.advance(if fp.kind == FileKind::File { fp.len } else { 0 }, 1),
                // Already gone is the state this call wanted.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(DfError::io(&path, e)),
            }
        }
        Ok(())
    }

    /// What is left of this manifest after a [`remove`](CopyManifest::remove)
    /// that stopped part way, so a second `u` can finish the job.
    ///
    /// Only the paths still on disk survive, and each surviving directory's
    /// recorded child count is recomputed *from the manifest* rather than from
    /// the disk — so the retry still refuses if somebody has put something new
    /// in the half-deleted tree. `None` when nothing is left to delete.
    pub fn remaining(&self) -> Option<CopyManifest> {
        let mut entries: Vec<(PathBuf, Fingerprint)> = self
            .entries
            .iter()
            .filter(|(rel, _)| exists(&self.path_of(rel)))
            .cloned()
            .collect();
        if entries.is_empty() {
            return None;
        }
        let mut children: HashMap<PathBuf, u64> = HashMap::new();
        for (rel, _) in &entries {
            if let Some(parent) = rel.parent() {
                *children.entry(parent.to_path_buf()).or_insert(0) += 1;
            }
        }
        for (rel, fp) in entries.iter_mut() {
            if fp.kind == FileKind::Dir {
                fp.entries = Some(children.get(rel).copied().unwrap_or(0));
            }
        }
        Some(CopyManifest {
            root: self.root.clone(),
            entries,
        })
    }
}

/// Depth-first, parents first, one entry per created path.
fn walk(
    path: &Path,
    rel: PathBuf,
    out: &mut Vec<(PathBuf, Fingerprint)>,
    cap: usize,
) -> Result<()> {
    if out.len() >= cap {
        return Err(DfError::Op(format!(
            "{}: more than {} files, too large to record an undo for",
            path.display(),
            grouped(cap as u64)
        )));
    }
    let fp = Fingerprint::of(path)?;
    let is_dir = fp.kind == FileKind::Dir;
    out.push((rel.clone(), fp));
    if !is_dir {
        return Ok(());
    }
    for entry in std::fs::read_dir(path).map_err(|e| DfError::io(path, e))? {
        let entry = entry.map_err(|e| DfError::io(path, e))?;
        walk(&entry.path(), rel.join(entry.file_name()), out, cap)?;
    }
    Ok(())
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

/// One link a link operation created, and what its inverse needs.
///
/// The same three fields [`OpRecord::Link`] carries, factored out so that
/// [`OpRecord::Links`] can hold a whole gesture's worth of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedLink {
    pub link: PathBuf,
    /// The link text, for a symlink. `None` for a hard link.
    pub target: Option<PathBuf>,
    pub fingerprint: Fingerprint,
}

impl CreatedLink {
    /// Fingerprint a link that has just been made. `target` is the text
    /// [`super::link::symlink`] returned, or `None` for a hard link.
    pub fn record(link: &Path, target: Option<&Path>) -> Result<CreatedLink> {
        Ok(CreatedLink {
            link: normalize(link),
            target: target.map(PathBuf::from),
            fingerprint: Fingerprint::of(link)?,
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
    /// `r` on a multi-selection: the bulk-rename diff view's whole card,
    /// committed as one gesture (PLAN §5).
    ///
    /// The same argument [`OpRecord::Links`] makes, and it is the reason this
    /// is not just a `Vec` inside [`OpRecord::Rename`]. Editing forty names in
    /// one card is *one* thing the user did, so it has to be one `u` — forty
    /// records would mean forty presses to take back one Enter, and
    /// thirty-nine intermediate states in which half the directory is renamed
    /// and half is not. The renames are also verified as a set before any of
    /// them is undone, so a card that half-committed is put back whole or not
    /// at all.
    ///
    /// Distinct from [`OpRecord::Move`], which has the identical shape, purely
    /// so the toast says "renamed 40 items" — a bulk rename that reported
    /// itself as a move would describe an operation the user did not perform.
    Renames { moved: Vec<MovedPath> },
    /// A yank-and-paste. Inverse: delete what was created, and nothing else —
    /// one [`CopyManifest`] per item that landed, every path in it verified
    /// before any of it is removed.
    Copy { created: Vec<CopyManifest> },
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
    /// `-` / `_` / `Ctrl+-` over a single file. Inverse: unlink.
    Link {
        link: PathBuf,
        /// The link text, for a symlink. `None` for a hard link.
        target: Option<PathBuf>,
        fingerprint: Fingerprint,
    },
    /// `-` / `_` / `Ctrl+-` over a yank of several files. Inverse: unlink every
    /// one of them.
    ///
    /// A separate variant rather than a `Vec` inside [`OpRecord::Link`]: the
    /// single-link shape is what a caller with one path in hand already builds,
    /// and widening it would churn every one of those call sites to say
    /// `vec![…]`. What earns the variant is the *undo*: linking eight yanked
    /// files is one gesture, so it has to be one `u` — eight records would mean
    /// eight presses to take back one keystroke, and seven intermediate states
    /// in which the directory holds some of the links and not the others.
    ///
    /// Every link is verified before any of them is removed, like
    /// [`OpRecord::Copy`], and a failure part way rewrites the record to cover
    /// only what is left so a second `u` finishes the job.
    Links { links: Vec<CreatedLink> },
}

impl OpRecord {
    /// One line for the `u` toast, in the past tense of what would happen.
    pub fn describe(&self) -> String {
        fn plural(n: usize, one: &str, many: &str) -> String {
            if n == 1 {
                format!("1 {one}")
            } else {
                format!("{} {many}", grouped(n as u64))
            }
        }
        match self {
            OpRecord::Move { moves } => format!("moved {}", plural(moves.len(), "item", "items")),
            OpRecord::Rename { moved } => format!(
                "renamed {}",
                moved.to.file_name().unwrap_or_default().to_string_lossy()
            ),
            OpRecord::Renames { moved } => match moved.as_slice() {
                [one] => format!(
                    "renamed {}",
                    one.to.file_name().unwrap_or_default().to_string_lossy()
                ),
                many => format!("renamed {}", plural(many.len(), "item", "items")),
            },
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
            OpRecord::Links { links } => match links.as_slice() {
                [one] => format!(
                    "linked {}",
                    one.link.file_name().unwrap_or_default().to_string_lossy()
                ),
                many => format!("linked {}", plural(many.len(), "item", "items")),
            },
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
    /// press `u` again. If the undo failed *part way through* a multi-item
    /// entry, the entry is rewritten to cover only what has not been taken back
    /// yet — otherwise the second `u` would trip over its own first half ("that
    /// file exists again") and the rest could never be undone at all.
    pub fn undo(&mut self, ctx: &TaskCtx) -> Result<UndoReport> {
        let Some(entry) = self.entries.back().cloned() else {
            return Err(DfError::Op("nothing to undo".to_string()));
        };
        let attempt = undo_attempt(&entry, ctx);
        match attempt.result {
            Ok(report) => {
                self.entries.pop_back();
                Ok(report)
            }
            Err(e) => {
                if let Some(remaining) = attempt.remaining {
                    if let Some(back) = self.entries.back_mut() {
                        *back = remaining;
                    }
                }
                Err(e)
            }
        }
    }
}

/// The result of one inverse, plus what is left of it if it stopped part way.
#[derive(Debug)]
pub struct UndoAttempt {
    pub result: Result<UndoReport>,
    /// A record covering only the items still to be undone. `Some` only when
    /// `result` is an error *and* some of the work was already taken back, so
    /// the entry the caller is holding no longer describes the world.
    pub remaining: Option<OpRecord>,
}

/// Run one inverse. Public so a caller can undo a record it is holding without
/// a journal — the conflict dialog's "put that back" path.
///
/// A caller that keeps the record around should use [`undo_attempt`] instead,
/// so a partial failure can rewrite it; this shorthand throws the remainder
/// away.
pub fn undo_record(record: &OpRecord, ctx: &TaskCtx) -> Result<UndoReport> {
    undo_attempt(record, ctx).result
}

/// [`undo_record`], keeping what is left over when it fails half way.
pub fn undo_attempt(record: &OpRecord, ctx: &TaskCtx) -> UndoAttempt {
    fn whole(result: Result<UndoReport>) -> UndoAttempt {
        UndoAttempt {
            result,
            remaining: None,
        }
    }
    match record {
        OpRecord::Move { moves } => {
            undo_moves(moves, ctx, "Moved", |rest| OpRecord::Move { moves: rest })
        }
        OpRecord::Rename { moved } => undo_moves(
            std::slice::from_ref(moved),
            ctx,
            "Renamed",
            // A rename is one leg; there is no "part way" for it to stop at.
            |rest| OpRecord::Move { moves: rest },
        ),
        // A bulk rename *can* stop part way, and what is left rebuilds as
        // another bulk rename so a second `u` finishes the job.
        OpRecord::Renames { moved } => undo_moves(moved, ctx, "Renamed", |rest| {
            OpRecord::Renames { moved: rest }
        }),
        OpRecord::Copy { created } => undo_copy(created, ctx),
        OpRecord::Trash { items } => whole(undo_trash(items, ctx)),
        OpRecord::Create {
            path,
            is_dir,
            fingerprint,
            created_parents,
        } => whole(undo_create(path, *is_dir, fingerprint, created_parents)),
        OpRecord::Link {
            link,
            target,
            fingerprint,
        } => whole(undo_link(link, target.as_deref(), fingerprint)),
        OpRecord::Links { links } => undo_links(links, ctx),
    }
}

/// Move everything back where it came from.
///
/// Checked in full first: a five-file paste that can only put three back should
/// put none back, rather than leaving the user with a half-undone state they
/// now have to reason about.
fn undo_moves(
    moves: &[MovedPath],
    ctx: &TaskCtx,
    verb: &str,
    rebuild: impl Fn(Vec<MovedPath>) -> OpRecord,
) -> UndoAttempt {
    let refuse = |e: DfError| UndoAttempt {
        result: Err(e),
        remaining: None,
    };
    for m in moves {
        if let Err(e) = m.fingerprint.verify(&m.to) {
            return refuse(e);
        }
        if exists(&m.from) {
            return refuse(DfError::Op(format!(
                "cannot undo: {} exists again",
                m.from.display()
            )));
        }
        let Some(parent) = m.from.parent() else {
            return refuse(DfError::Op(format!("{} has no parent", m.from.display())));
        };
        if !exists(parent) {
            return refuse(DfError::Op(format!(
                "cannot undo: {} no longer exists",
                parent.display()
            )));
        }
    }

    let mut touched = Vec::new();
    for (i, m) in moves.iter().enumerate() {
        if let Err(e) = super::copy::move_path(&m.to, &m.from, ctx, false) {
            // Everything before `i` is already back where it came from, so the
            // entry as it stands would refuse for ever. Keep the remainder.
            return UndoAttempt {
                result: Err(e),
                remaining: (i > 0).then(|| rebuild(moves[i..].to_vec())),
            };
        }
        touched.push(m.from.clone());
    }
    let what = if moves.len() == 1 {
        moves[0]
            .from
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    } else {
        format!("{} items", grouped(moves.len() as u64))
    };
    UndoAttempt {
        result: Ok(UndoReport {
            description: format!("{verb} {what} back"),
            touched,
        }),
        remaining: None,
    }
}

/// Delete exactly what the copy created, and only if every bit of it is
/// untouched.
///
/// The whole manifest of every item is verified before a single path is
/// removed: a paste of four folders that can only take three of them back
/// should take none, rather than leaving the user half-way between two states.
fn undo_copy(created: &[CopyManifest], ctx: &TaskCtx) -> UndoAttempt {
    for manifest in created {
        if let Err(e) = manifest.verify() {
            return UndoAttempt {
                result: Err(e),
                remaining: None,
            };
        }
    }
    let mut touched = Vec::new();
    for (i, manifest) in created.iter().enumerate() {
        if let Err(e) = manifest.remove(ctx) {
            // This item is part-deleted and the ones after it are untouched.
            // Re-record both so a second `u` finishes the job instead of
            // refusing over the half that already went.
            let mut rest: Vec<CopyManifest> = manifest.remaining().into_iter().collect();
            rest.extend(created[i + 1..].iter().cloned());
            let remaining = (!rest.is_empty()).then_some(OpRecord::Copy { created: rest });
            return UndoAttempt {
                result: Err(e),
                remaining,
            };
        }
        touched.push(manifest.root.clone());
    }
    UndoAttempt {
        result: Ok(UndoReport {
            description: if created.len() == 1 {
                format!(
                    "Removed the copy {}",
                    created[0]
                        .root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            } else {
                format!("Removed {} copies", grouped(created.len() as u64))
            },
            touched,
        }),
        remaining: None,
    }
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
                items[0]
                    .original
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            )
        } else {
            format!(
                "Restored {} items from the trash",
                grouped(items.len() as u64)
            )
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

/// Unlink every link one gesture made, all-or-nothing on the check.
///
/// Verified in full before anything is removed, for the same reason
/// [`undo_moves`] is: a `-` over eight yanked files that can only take six of
/// them back should take none, rather than leaving a directory with two links
/// in it that the user now has to reason about. If a removal fails *after* some
/// have gone, the remainder is handed back so a second `u` finishes rather than
/// tripping over the half that already went.
fn undo_links(links: &[CreatedLink], ctx: &TaskCtx) -> UndoAttempt {
    let refuse = |e: DfError| UndoAttempt {
        result: Err(e),
        remaining: None,
    };
    if links.is_empty() {
        return refuse(DfError::Op("nothing to undo".to_string()));
    }
    for l in links {
        if let Err(e) = l.fingerprint.verify(&l.link) {
            return refuse(e);
        }
        if let Some(target) = &l.target {
            match std::fs::read_link(&l.link) {
                Ok(now) if &now != target => {
                    return refuse(DfError::Op(format!(
                        "cannot undo: {} points somewhere else now",
                        l.link.display()
                    )))
                }
                Ok(_) => {}
                Err(e) => return refuse(DfError::io(&l.link, e)),
            }
        }
    }

    let mut touched = Vec::new();
    for (i, l) in links.iter().enumerate() {
        if let Err(e) = ctx.checkpoint() {
            return UndoAttempt {
                result: Err(e),
                remaining: (i > 0).then(|| OpRecord::Links {
                    links: links[i..].to_vec(),
                }),
            };
        }
        if let Err(e) = std::fs::remove_file(&l.link) {
            return UndoAttempt {
                result: Err(DfError::io(&l.link, e)),
                remaining: (i > 0).then(|| OpRecord::Links {
                    links: links[i..].to_vec(),
                }),
            };
        }
        ctx.advance(0, 1);
        touched.push(l.link.clone());
    }
    UndoAttempt {
        result: Ok(UndoReport {
            description: if links.len() == 1 {
                format!(
                    "Removed the link {}",
                    links[0]
                        .link
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            } else {
                format!("Removed {} links", grouped(links.len() as u64))
            },
            touched,
        }),
        remaining: None,
    }
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

    /// A bulk rename is one gesture, so it is one `u` — and the undo puts
    /// every name back or none of them (PLAN §5).
    #[test]
    fn undo_of_a_bulk_rename_puts_every_name_back_at_once() {
        let t = TempTree::new("j-bulk-rename");
        let mut moved = Vec::new();
        for (old, new) in [("a.txt", "1.txt"), ("b.txt", "2.txt"), ("c.txt", "3.txt")] {
            let from = t.file(old, b"x");
            let to = t.join(new);
            create::rename(&from, &to, false).unwrap();
            moved.push(MovedPath::record(&from, &to).unwrap());
        }

        let mut j = Journal::default();
        j.record(OpRecord::Renames {
            moved: moved.clone(),
        });
        assert_eq!(
            OpRecord::Renames {
                moved: moved.clone()
            }
            .describe(),
            "renamed 3 items"
        );

        let report = j.undo(&ctx()).unwrap();
        assert!(report.description.starts_with("Renamed"), "{report:?}");
        assert!(j.is_empty(), "one gesture, one undo");
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert!(t.join(name).is_file(), "{name} is not back");
        }
        for name in ["1.txt", "2.txt", "3.txt"] {
            assert!(!super::exists(&t.join(name)), "{name} is still there");
        }
    }

    /// A single-row card still reads as one file, not as "1 item".
    #[test]
    fn a_one_row_bulk_rename_names_the_file() {
        let t = TempTree::new("j-bulk-rename-one");
        let from = t.file("only.txt", b"x");
        let to = t.join("renamed.txt");
        create::rename(&from, &to, false).unwrap();
        let record = OpRecord::Renames {
            moved: vec![MovedPath::record(&from, &to).unwrap()],
        };
        assert_eq!(record.describe(), "renamed renamed.txt");
    }

    /// The journal entry a real paste of `dst` would produce.
    fn copied(dst: &Path) -> OpRecord {
        OpRecord::Copy {
            created: vec![CopyManifest::of_tree(dst).unwrap()],
        }
    }

    #[test]
    fn undo_of_a_copy_deletes_the_copies_only() {
        let t = TempTree::new("j-copy");
        let src = t.file("src/a.txt", b"data");
        let dst = t.join("dst-a.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(copied(&dst));
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
        j.record(copied(&dst));
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
        j.record(copied(&dst));
        std::fs::write(dst.join("new-work"), b"mine").unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert!(dst.join("new-work").is_file());
    }

    #[test]
    fn undo_of_a_copied_directory_refuses_new_contents_nested_below_it() {
        // BUG: `Fingerprint` describes one path. For a copied *tree* only the
        // top directory was fingerprinted, and its child count does not move
        // when work is added two levels down — so `u` walked in and
        // `remove_tree`d the lot: press u, lose today's work. The copy now
        // records a manifest of every path it created and the undo verifies the
        // whole of it, including that no directory holds anything the copy did
        // not put there.
        let t = TempTree::new("j-copy-dir-nested");
        let src = t.dir("src/sub");
        std::fs::write(src.join("a"), b"a").unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&t.join("src"), &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(copied(&dst));
        // New work, nested — the top directory still has exactly one child.
        std::fs::write(dst.join("sub/mine"), b"hours of work").unwrap();

        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert!(
            dst.join("sub/mine").is_file(),
            "newer work is never destroyed"
        );
        assert!(
            dst.join("sub/a").is_file(),
            "and neither is the copy itself"
        );
        assert_eq!(j.len(), 1, "the entry survives a refused undo");
    }

    #[test]
    fn undo_of_a_deep_copy_refuses_a_file_edited_deep_inside_it() {
        // The other half of the nested case: nothing was added or removed, so
        // every child count matches — one file three levels down was edited.
        let t = TempTree::new("j-copy-deep-edit");
        let src = t.dir("src/a/b/c");
        std::fs::write(src.join("notes.txt"), b"original").unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&t.join("src"), &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(copied(&dst));
        std::fs::write(dst.join("a/b/c/notes.txt"), b"a whole afternoon of work").unwrap();

        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert_eq!(
            std::fs::read(dst.join("a/b/c/notes.txt")).unwrap(),
            b"a whole afternoon of work"
        );
    }

    #[test]
    fn undo_of_a_copied_tree_removes_every_path_it_made() {
        let t = TempTree::new("j-copy-tree");
        let src = t.dir("src/sub/deeper");
        std::fs::write(src.join("f"), b"f").unwrap();
        std::fs::write(t.join("src/top"), b"top").unwrap();
        std::os::unix::fs::symlink("top", t.join("src/link")).unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&t.join("src"), &dst, &ctx(), false).unwrap();

        let record = copied(&dst);
        let OpRecord::Copy { created } = &record else {
            panic!("{record:?}")
        };
        assert_eq!(
            created[0].len(),
            6,
            "dst, sub, deeper, f, top, link: {:?}",
            created[0].paths().collect::<Vec<_>>()
        );

        let mut j = Journal::default();
        j.record(record);
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&dst), "children went before their parents");
        assert!(
            t.join("src/sub/deeper/f").is_file(),
            "the source is untouched"
        );
    }

    #[test]
    fn undo_of_a_copy_refuses_a_new_directory_nested_inside_it() {
        let t = TempTree::new("j-copy-new-dir");
        let src = t.dir("src/sub");
        std::fs::write(src.join("a"), b"a").unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&t.join("src"), &dst, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(copied(&dst));
        std::fs::create_dir(dst.join("sub/mine")).unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot undo"), "{err}");
        assert!(dst.join("sub/mine").is_dir());
    }

    #[test]
    fn a_partly_undone_copy_leaves_only_the_remainder_to_retry() {
        // A multi-item paste whose second item cannot be deleted: the first is
        // gone, so re-running the *original* entry would refuse for ever ("the
        // first copy is no longer there"). The entry is rewritten to the part
        // that is still outstanding.
        let t = TempTree::new("j-copy-partial");
        let src_file = t.file("src/a.txt", b"a");
        let src_dir = t.dir("src/folder");
        std::fs::write(src_dir.join("inner"), b"inner").unwrap();
        let dest = t.dir("dst");
        let copied_file = dest.join("a.txt");
        let copied_dir = dest.join("folder");
        copy::copy_tree(&src_file, &copied_file, &ctx(), false).unwrap();
        copy::copy_tree(&src_dir, &copied_dir, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![
                CopyManifest::of_tree(&copied_file).unwrap(),
                CopyManifest::of_tree(&copied_dir).unwrap(),
            ],
        });

        // Nothing may be unlinked from a directory with no write bit, so
        // removing `folder/inner` fails while `a.txt` has already gone.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&copied_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let err = j.undo(&ctx()).unwrap_err();
        std::fs::set_permissions(&copied_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(!err.to_string().is_empty(), "{err}");
        assert!(!super::exists(&copied_file), "the first item did go");
        assert!(copied_dir.join("inner").is_file(), "the second one did not");
        assert_eq!(j.len(), 1);
        match j.peek() {
            Some(OpRecord::Copy { created }) => {
                assert_eq!(created.len(), 1, "only the remainder is left to undo");
                assert_eq!(created[0].root, super::normalize(&copied_dir));
            }
            other => panic!("{other:?}"),
        }

        // A second `u` finishes the job rather than refusing over the half
        // that already went.
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&copied_dir));
        assert!(j.is_empty());
        assert_eq!(std::fs::read(src_dir.join("inner")).unwrap(), b"inner");
    }

    #[test]
    fn a_partly_undone_move_leaves_only_the_remainder_to_retry() {
        // BUG: an undo that failed part way through left an entry describing
        // moves that had already been undone, so every later `u` refused with
        // "exists again" and the rest could never be put back at all.
        let t = TempTree::new("j-move-partial");
        let a = t.file("one/a.txt", b"a");
        let b = t.file("two/b.txt", b"b");
        let dest = t.dir("dst");
        let a2 = dest.join("a.txt");
        let b2 = dest.join("b.txt");
        copy::move_path(&a, &a2, &ctx(), false).unwrap();
        copy::move_path(&b, &b2, &ctx(), false).unwrap();

        let mut j = Journal::default();
        j.record(OpRecord::Move {
            moves: vec![
                MovedPath::record(&a, &a2).unwrap(),
                MovedPath::record(&b, &b2).unwrap(),
            ],
        });

        // `two/` cannot be written to, so `b.txt` cannot be moved back into it
        // — but `a.txt` already has been.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(t.join("two"), std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(j.undo(&ctx()).is_err());
        std::fs::set_permissions(t.join("two"), std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(std::fs::read(&a).unwrap(), b"a", "the first leg went back");
        assert!(super::exists(&b2), "the second one did not");
        assert_eq!(j.len(), 1);
        match j.peek() {
            Some(OpRecord::Move { moves }) => {
                assert_eq!(moves.len(), 1, "only the leg still outstanding");
                assert_eq!(moves[0].to, super::normalize(&b2));
            }
            other => panic!("{other:?}"),
        }

        j.undo(&ctx()).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        assert!(j.is_empty());
    }

    #[test]
    fn a_tree_too_big_to_record_is_simply_not_undoable() {
        // The memory bound: past the cap the copy is not journalled at all,
        // rather than the journal holding a hundred megabytes of paths — or,
        // far worse, `u` deleting a 200 000-file tree on the strength of a
        // record that could not describe it.
        let t = TempTree::new("j-manifest-cap");
        let dst = t.dir("dst");
        for i in 0..5 {
            std::fs::write(dst.join(format!("f{i}")), b"x").unwrap();
        }
        let err = CopyManifest::of_tree_capped(&dst, 3).unwrap_err();
        assert!(err.to_string().contains("too large to record"), "{err}");
        // Under the cap it records the lot.
        assert_eq!(CopyManifest::of_tree_capped(&dst, 64).unwrap().len(), 6);
    }

    #[test]
    fn a_manifest_is_stored_relative_to_its_root() {
        // The memory story: one absolute path, then names.
        let t = TempTree::new("j-manifest-rel");
        let dir = t.dir("a/very/long/prefix/dst/sub");
        std::fs::write(dir.join("f"), b"f").unwrap();
        let root = t.join("a/very/long/prefix/dst");
        let m = CopyManifest::of_tree(&root).unwrap();
        assert_eq!(m.root, super::normalize(&root));
        assert_eq!(
            m.paths().collect::<Vec<_>>(),
            vec![root.clone(), root.join("sub"), root.join("sub/f")]
        );
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

    /// Linking a yank of several files is one gesture, so it is one `u`.
    #[test]
    fn undo_of_a_batch_of_links_takes_them_all_back_at_once() {
        let t = TempTree::new("j-links");
        let mut links = Vec::new();
        for name in ["a.txt", "ünïcödé — 日本語 🎬.txt", "with spaces.txt"] {
            let target = t.file(name, b"x");
            let l = t.join(format!("link-{name}"));
            let text = link::symlink(&target, &l, LinkKind::Absolute).unwrap();
            links.push(CreatedLink::record(&l, Some(&text)).unwrap());
        }
        // One hard link in the same gesture: `target: None` is the other shape.
        let hard_target = t.file("hard-target", b"x");
        let hard = t.join("hard-link");
        link::hardlink(&hard_target, &hard).unwrap();
        links.push(CreatedLink::record(&hard, None).unwrap());

        let record = OpRecord::Links {
            links: links.clone(),
        };
        assert_eq!(record.describe(), "linked 4 items");

        let mut j = Journal::default();
        j.record(record);
        assert_eq!(j.len(), 1, "one record, not four");
        let report = j.undo(&ctx()).unwrap();
        assert_eq!(report.touched.len(), 4);
        for l in &links {
            assert!(!super::exists(&l.link), "{}", l.link.display());
        }
        assert!(j.is_empty(), "one `u` emptied it");
        assert!(hard_target.is_file(), "the targets are untouched");
    }

    /// All-or-nothing on the check: one link pointing somewhere else now
    /// refuses the whole batch, and nothing is removed.
    #[test]
    fn undo_of_a_batch_refuses_whole_when_one_link_changed() {
        let t = TempTree::new("j-links-changed");
        let target = t.file("target.txt", b"x");
        let mut links = Vec::new();
        for name in ["one", "two"] {
            let l = t.join(name);
            let text = link::symlink(&target, &l, LinkKind::Absolute).unwrap();
            links.push(CreatedLink::record(&l, Some(&text)).unwrap());
        }
        let mut j = Journal::default();
        j.record(OpRecord::Links {
            links: links.clone(),
        });

        std::fs::remove_file(t.join("two")).unwrap();
        std::os::unix::fs::symlink("/somewhere/else", t.join("two")).unwrap();

        let err = j.undo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("points somewhere else"), "{err}");
        assert!(
            super::exists(&links[0].link),
            "the untouched link is still there: nothing was removed"
        );
        assert_eq!(j.len(), 1, "the entry stays so it can be retried");
    }

    /// A single-item batch reads like the single-link record it replaces.
    #[test]
    fn a_batch_of_one_describes_itself_by_name() {
        let t = TempTree::new("j-links-one");
        let target = t.file("target.txt", b"x");
        let l = t.join("just-one");
        let text = link::symlink(&target, &l, LinkKind::Absolute).unwrap();
        let record = OpRecord::Links {
            links: vec![CreatedLink::record(&l, Some(&text)).unwrap()],
        };
        assert_eq!(record.describe(), "linked just-one");
        let report = undo_record(&record, &ctx()).unwrap();
        assert_eq!(report.description, "Removed the link just-one");
        assert!(!super::exists(&l));
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
                created: vec![CopyManifest::of_tree(&p).unwrap()]
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
