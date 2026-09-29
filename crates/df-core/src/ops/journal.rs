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
//! ## Redo
//!
//! `U` walks forward again. An undo that took its entry back whole keeps it —
//! the record, and the state of every path it put back ([`Undone`]) — on a
//! second stack, and a redo repeats the operation from there *after the same
//! kind of check*: every path the undo restored must still be what the undo
//! left, and every path the operation will make must still be free. A mismatch
//! is an error the user reads, exactly as it is for `u`. What was done again
//! goes back on the undo stack, so `u`, `U`, `u` walks back and forth over one
//! step for as long as nothing else happens.
//!
//! A change of tags or of permissions changes nothing's name, so there its
//! check is the value itself: every file still carries exactly the tags the
//! undo put back, or every path still has exactly the `st_mode` it did — and
//! is still the same inode, found from the folder the change was asked in
//! without following a link, as [`super::mode`] finds it.
//!
//! A copy is the one redo that is not done here. It can take minutes, so it is
//! checked here and handed back as a plan ([`RedoCopy`]) for the caller to run
//! as the job a paste is, with its row in the task panel, its progress and its
//! cancel; the journal holds the entry aside while it runs, refuses `u` and
//! `U` until it is over, and is told how it went ([`Journal::finish_redo`]).
//!
//! The redo stack is a line, not a tree. Recording anything new clears it: a
//! redo of something undone before today's rename would replay it onto a
//! world it was never made in. So does an undo that stopped part way, whose
//! remainder is still on the undo stack and whose half that went has no
//! forward form of its own to be redone from.
//!
//! One thing is deliberately absent: **permanent delete is not journalled.**
//! There is nothing to record — the bytes are gone. It is the one
//! irreversible operation, which is why it is the one with a confirm dialog
//! (PLAN §5).

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use crate::tasks::TaskCtx;
use crate::text::grouped;
use crate::{DfError, Result};

use super::paste::{PasteItem, PasteMode, PastePlan};
use super::trash::{Trash, TrashedItem};
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
        self.check(path, "undo")
    }

    /// [`Fingerprint::verify`], for the step named by `verb` — `"undo"` or
    /// `"redo"` — which is the word the refusal starts with.
    pub fn check(&self, path: &Path, verb: &str) -> Result<()> {
        let now = Fingerprint::of(path).map_err(|_| {
            DfError::Op(format!(
                "cannot {verb}: {} is no longer there",
                path.display()
            ))
        })?;
        if now.kind != self.kind {
            return Err(DfError::Op(format!(
                "cannot {verb}: {} is not the same kind of file any more",
                path.display()
            )));
        }
        match self.kind {
            FileKind::Dir => {
                if now.entries != self.entries {
                    return Err(DfError::Op(format!(
                        "cannot {verb}: {} has different contents now",
                        path.display()
                    )));
                }
            }
            FileKind::File => {
                if now.len != self.len {
                    return Err(DfError::Op(format!(
                        "cannot {verb}: {} has changed size",
                        path.display()
                    )));
                }
                if let (Some(a), Some(b)) = (self.mtime, now.mtime) {
                    if a != b {
                        return Err(DfError::Op(format!(
                            "cannot {verb}: {} has been modified since",
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
    /// What the copy was made from, which is what `U` copies again once `u`
    /// has taken the copy away. A paste records it
    /// ([`CopyManifest::copied_from`]); an extraction has none — what it
    /// copied was inside an archive — and so cannot be redone.
    source: Option<CopySource>,
}

/// Where one copied item came from, and the state it was in when it was
/// copied.
///
/// Checked before a redo copies it again, for the reason every step in this
/// module checks what it is about to act on: a source edited since the paste
/// would be copied as it is *now*, and a redo that made something other than
/// what the undo took away would not be a redo.
///
/// The check is the source's own [`Fingerprint`] and no deeper: its kind, a
/// file's size and mtime, a folder's count of entries. A file edited, replaced
/// or truncated is caught; a file changed somewhere *inside* a copied folder
/// is not, and the redo copies that folder as it is now. A manifest of the
/// source, as a copy keeps of what it made, would catch it, at the cost of
/// walking the whole source at paste time for a redo that is rarely asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySource {
    pub path: PathBuf,
    pub fingerprint: Fingerprint,
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
        Ok(CopyManifest {
            root,
            entries,
            source: None,
        })
    }

    /// The same manifest, remembering that the copy was made from `source`
    /// ([`CopySource`]). A source that can no longer be read is left out, and
    /// with it the chance to redo this copy; the undo is unaffected.
    pub fn copied_from(mut self, source: &Path) -> CopyManifest {
        self.source = Fingerprint::of(source).ok().map(|fingerprint| CopySource {
            path: source.to_path_buf(),
            fingerprint,
        });
        self
    }

    /// What the copy was made from, when that is known.
    pub fn source(&self) -> Option<&CopySource> {
        self.source.as_ref()
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
            source: self.source.clone(),
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
    /// For a hard link, the file it is a second name for — what `U` links
    /// again once `u` has taken it away. `None` for a symlink, whose text says
    /// where it points, and for a hard link made without saying.
    pub original: Option<PathBuf>,
    pub fingerprint: Fingerprint,
}

impl CreatedLink {
    /// Fingerprint a link that has just been made. `target` is the text
    /// [`super::link::symlink`] returned, or `None` for a hard link.
    pub fn record(link: &Path, target: Option<&Path>) -> Result<CreatedLink> {
        Ok(CreatedLink {
            link: normalize(link),
            target: target.map(PathBuf::from),
            original: None,
            fingerprint: Fingerprint::of(link)?,
        })
    }

    /// Fingerprint a hard link that has just been made to `original`, keeping
    /// `original` so that the link can be made again.
    pub fn record_hard(link: &Path, original: &Path) -> Result<CreatedLink> {
        Ok(CreatedLink {
            original: Some(original.to_path_buf()),
            ..CreatedLink::record(link, None)?
        })
    }
}

/// One file's tags, before and after the `Tags:` prompt changed them.
///
/// Both lists whole, rather than what was added and what was taken away: the
/// undo puts `before` back exactly, order and spelling included, and checks
/// the file still has exactly `after` first — a difference could be replayed
/// backwards over a set that has changed since, and quietly undo somebody
/// else's edit along with this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagChange {
    pub path: PathBuf,
    pub before: Vec<String>,
    pub after: Vec<String>,
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
        /// For a hard link, the file it is a second name for: what `U` links
        /// again. `None` for a symlink ([`CreatedLink::original`]).
        original: Option<PathBuf>,
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
    /// `T`: the tags of one file, or of every item in a selection, as one
    /// `Enter` in the `Tags:` prompt changed them ([`crate::fs::tags`]).
    /// Inverse: put each file's old tags back — all of them, and only once
    /// every file is checked to still carry exactly what the prompt wrote.
    Tags { changes: Vec<TagChange> },
    /// `C`'s Apply, or a chip on the spot panel: the permissions of every path
    /// it changed, before and after ([`super::mode`]). Inverse: put each
    /// `before` back, once every path is seen to still have its `after`.
    ///
    /// One record for the whole card, a folder's tree included, for the
    /// reason [`OpRecord::Renames`] is one: it was one Apply. The shape is
    /// the plain pair per path so the way forward again is as obvious as the
    /// way back: `after` where `before` is found.
    Mode {
        changes: Vec<super::mode::ModeChange>,
    },
}

/// "1 item" / "3 items" / "1,234 items".
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", grouped(n as u64))
    }
}

/// A path's last component, for a sentence.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// The sentence with its first letter raised, for a toast: the records
/// describe themselves in the lower case a history row or a clause wants.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

impl OpRecord {
    /// One line for what the operation did, in the past tense: the undo
    /// history's name for a row `u` can take back.
    pub fn describe(&self) -> String {
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
            OpRecord::Tags { changes } => match changes.as_slice() {
                [one] => format!(
                    "tagged {}",
                    one.path.file_name().unwrap_or_default().to_string_lossy()
                ),
                many => format!("tagged {}", plural(many.len(), "item", "items")),
            },
            OpRecord::Mode { changes } => format!(
                "changed permissions of {}",
                plural(changes.len(), "item", "items")
            ),
        }
    }

    /// The same for doing it again, in the past tense of having done it: the
    /// `U` toast, and the undo history's name for a row `U` can redo.
    pub fn describe_redo(&self) -> String {
        match self {
            OpRecord::Move { moves } => match moves.as_slice() {
                [one] => format!("moved {} again", name_of(&one.to)),
                many => format!("moved {} again", plural(many.len(), "item", "items")),
            },
            OpRecord::Rename { moved } => {
                format!(
                    "renamed {} → {} again",
                    name_of(&moved.from),
                    name_of(&moved.to)
                )
            }
            OpRecord::Renames { moved } => match moved.as_slice() {
                [one] => format!(
                    "renamed {} → {} again",
                    name_of(&one.from),
                    name_of(&one.to)
                ),
                many => format!("renamed {} again", plural(many.len(), "item", "items")),
            },
            OpRecord::Copy { created } => match created.as_slice() {
                [one] => format!("copied {} again", name_of(&one.root)),
                many => format!("copied {} again", plural(many.len(), "item", "items")),
            },
            OpRecord::Trash { items } => match items.as_slice() {
                [one] => format!("trashed {} again", name_of(&one.original)),
                many => format!("trashed {} again", plural(many.len(), "item", "items")),
            },
            OpRecord::Create { path, is_dir, .. } => format!(
                "created {} {} again",
                if *is_dir { "folder" } else { "file" },
                name_of(path)
            ),
            OpRecord::Link { link, .. } => format!("linked {} again", name_of(link)),
            OpRecord::Links { links } => match links.as_slice() {
                [one] => format!("linked {} again", name_of(&one.link)),
                many => format!("linked {} again", plural(many.len(), "item", "items")),
            },
            OpRecord::Tags { changes } => match changes.as_slice() {
                [one] => format!("tagged {} again", name_of(&one.path)),
                many => format!("tagged {} again", plural(many.len(), "item", "items")),
            },
            OpRecord::Mode { changes } => format!(
                "changed permissions of {} again",
                plural(changes.len(), "item", "items")
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

/// What a redo did, for the toast: the sentence, and the paths it made,
/// moved or trashed again — whatever the UI should move the cursor to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedoReport {
    pub description: String,
    pub touched: Vec<PathBuf>,
}

/// An operation `u` took back, kept so that `U` can do it again.
///
/// The record is the operation as it was first done, which is what a redo
/// repeats and what the undo history names. `restored` is the other half of
/// the check every step in this module makes before it touches anything: each
/// path the undo *put back* — a file moved back to where it came from, one
/// restored out of the trash — and the state the undo left it in. A redo that
/// finds one of them changed refuses, as an undo does, rather than moving the
/// user's newer work somewhere they did not ask for it to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    pub record: OpRecord,
    restored: Vec<(PathBuf, Fingerprint)>,
}

impl Undone {
    /// What `u` leaves for `U` after taking `record` back and reporting
    /// `touched`: the fingerprint of every path it put back. Only the kinds
    /// whose undo restores something have any; a copy's, a create's and a
    /// link's undo only removed things, and their redo checks instead that the
    /// names are still free.
    fn after(record: OpRecord, touched: &[PathBuf]) -> Undone {
        let restores = matches!(
            record,
            OpRecord::Move { .. }
                | OpRecord::Rename { .. }
                | OpRecord::Renames { .. }
                | OpRecord::Trash { .. }
        );
        let restored = if restores {
            touched
                .iter()
                .filter_map(|path| Fingerprint::of(path).ok().map(|fp| (path.clone(), fp)))
                .collect()
        } else {
            Vec::new()
        };
        Undone { record, restored }
    }

    /// The state the undo left `path` in, if it was one it put back.
    fn restored(&self, path: &Path) -> Option<&Fingerprint> {
        self.restored
            .iter()
            .find(|(restored, _)| restored == path)
            .map(|(_, fingerprint)| fingerprint)
    }

    /// The part of this still to be redone after a redo stopped part way: the
    /// same fingerprints, for a record covering only what is left.
    fn rest(&self, record: OpRecord) -> Undone {
        Undone {
            record,
            restored: self.restored.clone(),
        }
    }
}

/// One entry on either stack, and when it was first done — for the undo
/// history's "3 min ago".
///
/// The stamp travels with the entry from one stack to the other and back, so
/// the history reads as one timeline whichever side of the line an entry is
/// on: an operation undone and redone happened when it happened.
#[derive(Debug, Clone)]
struct Stamped<T> {
    item: T,
    at: Instant,
}

/// The bounded stack of inverses, and the stack of what they took back.
#[derive(Debug, Clone)]
pub struct Journal {
    entries: VecDeque<Stamped<OpRecord>>,
    /// What `u` has taken back, the most recently undone last: `U` repeats
    /// from the back. Bounded by the same depth.
    undone: VecDeque<Stamped<Undone>>,
    /// A redo of a copy that is running as a job ([`RedoCopy`]), off the redo
    /// stack until it is over.
    copying: Option<Copying>,
    depth: usize,
}

/// A redo stack entry whose copy is running, held aside until the caller says
/// how it went ([`Journal::finish_redo`]).
#[derive(Debug, Clone)]
struct Copying {
    entry: Stamped<Undone>,
    /// Whether the redo stack it came off is still there to put it back on.
    /// Something new recorded while it ran clears that stack, and whatever of
    /// the copy does not land has no line left to wait in.
    line: bool,
}

/// What `U` did, or has handed back to be done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redo {
    /// Done, here and now.
    Done(RedoReport),
    /// A copy, checked and ready for the caller to run as a job.
    Copy(RedoCopy),
}

/// A copy `U` is to make again: checked — every source still the file it was
/// when it was copied, every destination still free — and handed back to be
/// run rather than run here, because a copy is the one redo that can take
/// minutes. The caller runs [`RedoCopy::plan`] as the job a paste is and then
/// tells the journal what landed ([`Journal::finish_redo`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedoCopy {
    /// Each item's source, and the destination it is copied to again.
    pub items: Vec<(PathBuf, PathBuf)>,
    /// What to say once it has landed: "Copied a.txt again".
    pub description: String,
}

impl RedoCopy {
    /// The copy as a settled paste: every item ready, none overwriting — each
    /// destination was checked free — and nothing left to ask about. The
    /// paste's own `execute` manifests each item as it lands, with its source,
    /// so what it records is what `u` takes back and `U` makes again.
    pub fn plan(&self) -> PastePlan {
        let dest_dir = self
            .items
            .first()
            .and_then(|(_, dst)| dst.parent())
            .map(Path::to_path_buf)
            .unwrap_or_default();
        PastePlan {
            mode: PasteMode::Copy,
            dest_dir,
            ready: self
                .items
                .iter()
                .map(|(src, dst)| PasteItem {
                    src: src.clone(),
                    dst: dst.clone(),
                    overwrite: false,
                })
                .collect(),
            conflicts: Vec::new(),
            no_ops: Vec::new(),
        }
    }
}

/// Why `u` and `U` wait while a redo's copy runs: the copy lands on the undo
/// stack when it is over, and a step taken meanwhile would put it there out
/// of order.
const COPYING: &str = "a copy being redone is still running";

impl Default for Journal {
    fn default() -> Journal {
        Journal::new(JOURNAL_DEPTH)
    }
}

impl Journal {
    pub fn new(depth: usize) -> Journal {
        Journal {
            entries: VecDeque::new(),
            undone: VecDeque::new(),
            copying: None,
            // A depth of 0 would silently make `u` do nothing; one step of undo
            // is the minimum that is not a lie.
            depth: depth.max(1),
        }
    }

    /// Push a completed operation. The oldest entry falls off the bottom once
    /// the journal is full, and whatever was undone before it can no longer
    /// be redone (see the module header).
    pub fn record(&mut self, record: OpRecord) {
        self.record_at(record, Instant::now());
    }

    /// [`Journal::record`], stamped `at` rather than now — for a caller that
    /// has to say how long ago.
    pub fn record_at(&mut self, record: OpRecord, at: Instant) {
        self.undone.clear();
        if let Some(copying) = &mut self.copying {
            copying.line = false;
        }
        self.push(record, at);
    }

    /// Onto the undo stack, leaving the redo stack alone: what a redo did
    /// again is not something new.
    fn push(&mut self, record: OpRecord, at: Instant) {
        self.entries.push_back(Stamped { item: record, at });
        while self.entries.len() > self.depth {
            self.entries.pop_front();
        }
    }

    fn push_undone(&mut self, undone: Undone, at: Instant) {
        self.undone.push_back(Stamped { item: undone, at });
        while self.undone.len() > self.depth {
            self.undone.pop_front();
        }
    }

    /// How many operations `u` can take back.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there is nothing for `u` to take back.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many operations `U` can do again.
    pub fn redo_len(&self) -> usize {
        self.undone.len()
    }

    /// Whether there is anything for `U` to do again.
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Whether a redo's copy is running ([`RedoCopy`]), and `u` and `U` are
    /// waiting for it.
    pub fn busy(&self) -> bool {
        self.copying.is_some()
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    /// What `u` would take back next, without taking it back.
    pub fn peek(&self) -> Option<&OpRecord> {
        self.entries.back().map(|entry| &entry.item)
    }

    /// What `U` would do again next, without doing it.
    pub fn peek_redo(&self) -> Option<&OpRecord> {
        self.undone.back().map(|entry| &entry.item.record)
    }

    /// What `u` can take back, the next one first, each with when it was
    /// done.
    pub fn undoable(&self) -> impl Iterator<Item = (&OpRecord, Instant)> + '_ {
        self.entries
            .iter()
            .rev()
            .map(|entry| (&entry.item, entry.at))
    }

    /// What `U` can do again, the next one first, each with when it was first
    /// done.
    pub fn redoable(&self) -> impl Iterator<Item = (&OpRecord, Instant)> + '_ {
        self.undone
            .iter()
            .rev()
            .map(|entry| (&entry.item.record, entry.at))
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.undone.clear();
        self.copying = None;
    }

    /// Undo the most recent operation.
    ///
    /// The entry stays on the stack if the undo fails, so the user can fix
    /// whatever is in the way (close the file, move the newer copy aside) and
    /// press `u` again. If the undo failed *part way through* a multi-item
    /// entry, the entry is rewritten to cover only what has not been taken back
    /// yet — otherwise the second `u` would trip over its own first half ("that
    /// file exists again") and the rest could never be undone at all.
    ///
    /// Taken back whole, the entry moves to the redo stack for `U`
    /// ([`Undone`]). Taken back half way, it clears that stack instead: the
    /// half that went has no forward form of its own, and nothing undone
    /// before it can be redone past it.
    pub fn undo(&mut self, ctx: &TaskCtx) -> Result<UndoReport> {
        if self.busy() {
            return Err(DfError::Op(COPYING.to_string()));
        }
        let Some(entry) = self.entries.back().cloned() else {
            return Err(DfError::Op("nothing to undo".to_string()));
        };
        let attempt = undo_attempt(&entry.item, ctx);
        match attempt.result {
            Ok(report) => {
                self.entries.pop_back();
                self.push_undone(Undone::after(entry.item, &report.touched), entry.at);
                Ok(report)
            }
            Err(e) => {
                if let Some(remaining) = attempt.remaining {
                    if let Some(back) = self.entries.back_mut() {
                        back.item = remaining;
                    }
                    self.undone.clear();
                }
                Err(e)
            }
        }
    }

    /// Do the most recently undone operation again.
    ///
    /// Checked in full before anything is touched, as an undo is, and the
    /// entry stays if the redo is refused, so the user can clear the way and
    /// press `U` again. What was done again goes back on the undo stack —
    /// without clearing this one, since it is not something new — so `u`
    /// takes it back. A redo that fails part way puts what did land on the
    /// undo stack and keeps the rest here, so neither half is lost.
    ///
    /// A copy is checked here and handed back ([`Redo::Copy`]) rather than
    /// made: the entry comes off this stack and is held aside until the caller
    /// has run it and said what landed ([`Journal::finish_redo`]).
    pub fn redo(&mut self, ctx: &TaskCtx) -> Result<Redo> {
        if self.busy() {
            return Err(DfError::Op(COPYING.to_string()));
        }
        let Some(entry) = self.undone.back().cloned() else {
            return Err(DfError::Op("nothing to redo".to_string()));
        };
        if let OpRecord::Copy { created } = &entry.item.record {
            let copy = redo_copy(&entry.item, created)?;
            self.undone.pop_back();
            self.copying = Some(Copying { entry, line: true });
            return Ok(Redo::Copy(copy));
        }
        let attempt = redo_attempt(&entry.item, ctx);
        if let Some(done) = attempt.done {
            self.push(done, entry.at);
        }
        match attempt.result {
            Ok(report) => {
                self.undone.pop_back();
                Ok(Redo::Done(report))
            }
            Err(e) => {
                if let Some(rest) = attempt.remaining {
                    if let Some(back) = self.undone.back_mut() {
                        back.item = rest;
                    }
                }
                Err(e)
            }
        }
    }

    /// A redo's copy is over — landed, failed or cancelled — and `landed` is
    /// the record of what it made: the paste's record, one manifest per item
    /// that landed whole, or `None` when nothing did.
    ///
    /// What landed goes on the undo stack, stamped with when the copy was
    /// first done, so `u` takes it back. What did not goes back on the redo
    /// stack to be tried again — the whole entry when nothing landed — unless
    /// something new was recorded while the copy ran, which has cleared the
    /// line it would have waited in.
    pub fn finish_redo(&mut self, landed: Option<OpRecord>) {
        let copying = self.copying.take();
        let at = copying
            .as_ref()
            .map_or_else(Instant::now, |copying| copying.entry.at);
        let roots: Vec<PathBuf> = match &landed {
            Some(OpRecord::Copy { created }) => created.iter().map(|m| m.root.clone()).collect(),
            _ => Vec::new(),
        };
        if let Some(record) = landed {
            self.push(record, at);
        }
        let Some(Copying { entry, line: true }) = copying else {
            return;
        };
        let OpRecord::Copy { created } = &entry.item.record else {
            return;
        };
        let rest: Vec<CopyManifest> = created
            .iter()
            .filter(|manifest| !roots.contains(&manifest.root))
            .cloned()
            .collect();
        if !rest.is_empty() {
            let undone = entry.item.rest(OpRecord::Copy { created: rest });
            self.push_undone(undone, entry.at);
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
        OpRecord::Trash { items } => undo_trash(items, ctx),
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
            ..
        } => whole(undo_link(link, target.as_deref(), fingerprint)),
        OpRecord::Links { links } => undo_links(links, ctx),
        OpRecord::Tags { changes } => undo_tags(changes),
        OpRecord::Mode { changes } => super::mode::undo(changes, ctx),
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

/// Restore everything a `d` trashed.
///
/// `restore` already refuses to overwrite; every item is checked first so a
/// multi-file `d` is put back all at once or not at all. If a restore fails
/// *after* some have gone back — a folder the file belongs in that can no
/// longer be written to — the items still in the trash are handed back as the
/// remainder, as [`undo_moves`] hands back its legs, so the entry stops
/// listing files that are no longer in the trash and a second `u` finishes.
fn undo_trash(items: &[TrashedItem], ctx: &TaskCtx) -> UndoAttempt {
    let refuse = |e: DfError| UndoAttempt {
        result: Err(e),
        remaining: None,
    };
    for item in items {
        if !exists(&item.files_path()) {
            return refuse(DfError::Op(format!(
                "cannot undo: {} is no longer in the trash",
                item.original.display()
            )));
        }
        if exists(&item.original) {
            return refuse(DfError::Op(format!(
                "cannot undo: {} exists again",
                item.original.display()
            )));
        }
    }
    let mut touched = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match super::trash::restore(item, ctx) {
            Ok(back) => touched.push(back),
            Err(e) => {
                return UndoAttempt {
                    result: Err(e),
                    remaining: (i > 0).then(|| OpRecord::Trash {
                        items: items[i..].to_vec(),
                    }),
                }
            }
        }
    }
    let result = Ok(UndoReport {
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
    });
    UndoAttempt {
        result,
        remaining: None,
    }
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

/// Put every file's old tags back, all-or-nothing on the check.
///
/// Each file must still carry exactly the tags the prompt wrote: a file whose
/// tags somebody has changed since — another `T`, another program — is a set
/// this undo knows nothing about, and restoring over it would throw that edit
/// away. One such file refuses the whole undo before any tag is touched, as a
/// multi-file move does. A write that fails part way hands back the rest, so a
/// second `u` finishes the job.
fn undo_tags(changes: &[TagChange]) -> UndoAttempt {
    use crate::fs::tags;
    let refuse = |e: DfError| UndoAttempt {
        result: Err(e),
        remaining: None,
    };
    if changes.is_empty() {
        return refuse(DfError::Op("nothing to undo".to_string()));
    }
    for change in changes {
        let now = match tags::try_read(&change.path) {
            Ok(now) if exists(&change.path) => now,
            _ => {
                return refuse(DfError::Op(format!(
                    "cannot undo: {} is no longer there",
                    change.path.display()
                )))
            }
        };
        if now != change.after {
            return refuse(DfError::Op(format!(
                "cannot undo: the tags of {} have changed since",
                change.path.display()
            )));
        }
    }
    let mut touched = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        if let Err(e) = tags::write(&change.path, &change.before) {
            return UndoAttempt {
                result: Err(DfError::Op(format!(
                    "cannot undo: {}: {e}",
                    change.path.display()
                ))),
                remaining: (i > 0).then(|| OpRecord::Tags {
                    changes: changes[i..].to_vec(),
                }),
            };
        }
        touched.push(change.path.clone());
    }
    UndoAttempt {
        result: Ok(UndoReport {
            description: match changes {
                [one] => format!(
                    "Restored tags of {}",
                    one.path.file_name().unwrap_or_default().to_string_lossy()
                ),
                many => format!("Restored tags of {} items", grouped(many.len() as u64)),
            },
            touched,
        }),
        remaining: None,
    }
}

// ── Redo ────────────────────────────────────────────────────────────────────

/// The result of doing one undone operation again.
#[derive(Debug)]
struct RedoAttempt {
    result: Result<RedoReport>,
    /// What was done again, as a record for the undo stack: all of it on
    /// success, and the part that landed when it stopped part way. `None`
    /// when nothing was done.
    done: Option<OpRecord>,
    /// What is left to redo, when it stopped part way. `None` when nothing
    /// changed, and the entry as it stands is still the truth.
    remaining: Option<Undone>,
}

impl RedoAttempt {
    /// Refused before anything was touched.
    fn refused(e: DfError) -> RedoAttempt {
        RedoAttempt {
            result: Err(e),
            done: None,
            remaining: None,
        }
    }
}

/// Do one undone operation again, having checked all of it first.
fn redo_attempt(undone: &Undone, ctx: &TaskCtx) -> RedoAttempt {
    match &undone.record {
        OpRecord::Move { moves } => redo_moves(undone, moves, ctx),
        OpRecord::Rename { moved } => redo_moves(undone, std::slice::from_ref(moved), ctx),
        OpRecord::Renames { moved } => redo_moves(undone, moved, ctx),
        OpRecord::Trash { items } => redo_trash(undone, items, ctx),
        // `Journal::redo` hands a copy back as a plan before it gets here.
        OpRecord::Copy { .. } => RedoAttempt::refused(DfError::Op(
            "cannot redo: a copy is made again as a job".to_string(),
        )),
        OpRecord::Create {
            path,
            is_dir,
            fingerprint,
            ..
        } => redo_create(undone, path, *is_dir, fingerprint),
        OpRecord::Link {
            link,
            target,
            original,
            fingerprint,
        } => {
            let one = CreatedLink {
                link: link.clone(),
                target: target.clone(),
                original: original.clone(),
                fingerprint: fingerprint.clone(),
            };
            redo_links(undone, std::slice::from_ref(&one), ctx)
        }
        OpRecord::Links { links } => redo_links(undone, links, ctx),
        OpRecord::Tags { changes } => redo_tags(undone, changes),
        OpRecord::Mode { changes } => {
            let redone = super::mode::redo(changes, ctx);
            RedoAttempt {
                result: redone.result.map(|()| RedoReport {
                    description: capitalised(&undone.record.describe_redo()),
                    touched: redone.done.iter().map(|c| c.path.clone()).collect(),
                }),
                done: (!redone.done.is_empty()).then_some(OpRecord::Mode {
                    changes: redone.done,
                }),
                remaining: (!redone.rest.is_empty()).then(|| {
                    undone.rest(OpRecord::Mode {
                        changes: redone.rest,
                    })
                }),
            }
        }
    }
}

/// The record a redo of `record` leaves on the undo stack for `moves`: the
/// same kind, so the toast after the next `u` still says "renamed".
fn moved_as(record: &OpRecord, mut moves: Vec<MovedPath>) -> OpRecord {
    match record {
        OpRecord::Rename { .. } if moves.len() == 1 => OpRecord::Rename {
            moved: moves.remove(0),
        },
        OpRecord::Renames { .. } => OpRecord::Renames { moved: moves },
        _ => OpRecord::Move { moves },
    }
}

/// Refuse unless `path`'s directory is still there to put something in.
fn parent_exists(path: &Path) -> Result<()> {
    match path.parent() {
        Some(parent) if exists(parent) => Ok(()),
        Some(parent) => Err(DfError::Op(format!(
            "cannot redo: {} no longer exists",
            parent.display()
        ))),
        None => Err(DfError::Op(format!("{} has no parent", path.display()))),
    }
}

/// Refuse unless `path` is free for the redo to make again.
fn free(path: &Path) -> Result<()> {
    if exists(path) {
        return Err(DfError::Op(format!(
            "cannot redo: {} is taken now",
            path.display()
        )));
    }
    parent_exists(path)
}

/// Move everything to where the operation took it, again.
///
/// All-or-nothing on the check, as [`undo_moves`] is: every leg's source must
/// be the file the undo put back, untouched since, and every destination
/// free, before the first one moves.
fn redo_moves(undone: &Undone, moves: &[MovedPath], ctx: &TaskCtx) -> RedoAttempt {
    for m in moves {
        let Some(fingerprint) = undone.restored(&m.from) else {
            return RedoAttempt::refused(DfError::Op(format!(
                "cannot redo: {} was not recorded when it was put back",
                m.from.display()
            )));
        };
        if let Err(e) = fingerprint
            .check(&m.from, "redo")
            .and_then(|()| free(&m.to))
        {
            return RedoAttempt::refused(e);
        }
    }

    let mut done = Vec::new();
    let mut touched = Vec::new();
    for (i, m) in moves.iter().enumerate() {
        if let Err(e) = super::copy::move_path(&m.from, &m.to, ctx, false) {
            // What moved is real and goes on the undo stack; what did not is
            // still to be redone, and nothing before it has to be done twice.
            return RedoAttempt {
                result: Err(e),
                done: (!done.is_empty()).then(|| moved_as(&undone.record, done)),
                remaining: (i > 0)
                    .then(|| undone.rest(moved_as(&undone.record, moves[i..].to_vec()))),
            };
        }
        match MovedPath::record(&m.from, &m.to) {
            Ok(leg) => done.push(leg),
            Err(e) => log::warn!("no undo record for {}: {e}", m.to.display()),
        }
        touched.push(m.to.clone());
    }
    RedoAttempt {
        result: Ok(RedoReport {
            description: capitalised(&undone.record.describe_redo()),
            touched,
        }),
        done: (!done.is_empty()).then(|| moved_as(&undone.record, done)),
        remaining: None,
    }
}

/// Put everything the undo restored back in the trash it came out of.
///
/// The same trash, not whichever [`super::trash::for_path`] would pick now:
/// it is the one the file went into the first time, and the one `u` will
/// look in to restore it again.
fn redo_trash(undone: &Undone, items: &[TrashedItem], ctx: &TaskCtx) -> RedoAttempt {
    for item in items {
        let Some(fingerprint) = undone.restored(&item.original) else {
            return RedoAttempt::refused(DfError::Op(format!(
                "cannot redo: {} was not recorded when it was restored",
                item.original.display()
            )));
        };
        if let Err(e) = fingerprint.check(&item.original, "redo") {
            return RedoAttempt::refused(e);
        }
    }

    let mut done = Vec::new();
    let mut touched = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match Trash::at(&item.trash_root).trash(&item.original, ctx) {
            Ok(again) => done.push(again),
            Err(e) => {
                return RedoAttempt {
                    result: Err(e),
                    done: (!done.is_empty()).then_some(OpRecord::Trash { items: done }),
                    remaining: (i > 0).then(|| {
                        undone.rest(OpRecord::Trash {
                            items: items[i..].to_vec(),
                        })
                    }),
                }
            }
        }
        touched.push(item.original.clone());
    }
    RedoAttempt {
        result: Ok(RedoReport {
            description: capitalised(&undone.record.describe_redo()),
            touched,
        }),
        done: (!done.is_empty()).then_some(OpRecord::Trash { items: done }),
        remaining: None,
    }
}

/// Check that every item can be copied again, from the source it was first
/// copied from, and say how.
///
/// Checked in full: each item has to have a recorded source ([`CopySource`])
/// whose fingerprint still matches, and a destination that is still free. The
/// copy itself is the caller's to run ([`RedoCopy`]).
fn redo_copy(undone: &Undone, created: &[CopyManifest]) -> Result<RedoCopy> {
    let mut items = Vec::with_capacity(created.len());
    for manifest in created {
        let Some(source) = manifest.source() else {
            return Err(DfError::Op(format!(
                "cannot redo: nothing records where {} came from",
                manifest.root.display()
            )));
        };
        source.fingerprint.check(&source.path, "redo")?;
        free(&manifest.root)?;
        items.push((source.path.clone(), manifest.root.clone()));
    }
    Ok(RedoCopy {
        items,
        description: capitalised(&undone.record.describe_redo()),
    })
}

/// Make the empty file or folder again, and whatever parents it needs.
///
/// Only an *empty* one: `a` makes something with nothing in it, and that is
/// the one kind of create there is a way to repeat. A file that was written as
/// it was made — an archive, the system clipboard pasted into a file — is
/// recorded as a create so that `u` can take it away, and nothing here holds
/// what was written into it.
fn redo_create(
    undone: &Undone,
    path: &Path,
    is_dir: bool,
    fingerprint: &Fingerprint,
) -> RedoAttempt {
    let empty = if is_dir {
        fingerprint.entries.unwrap_or(0) == 0
    } else {
        fingerprint.len == 0
    };
    if !empty {
        return RedoAttempt::refused(DfError::Op(format!(
            "cannot redo: {} was made with something in it, and only an empty one can be made again",
            name_of(path)
        )));
    }
    if exists(path) {
        return RedoAttempt::refused(DfError::Op(format!(
            "cannot redo: {} is taken now",
            path.display()
        )));
    }
    // `create`'s own spelling of "a directory": a trailing `/`. It makes the
    // parents that are missing, and says which, so `u` peels exactly those.
    let typed = if is_dir {
        let mut text = path.as_os_str().to_owned();
        text.push("/");
        PathBuf::from(text)
    } else {
        path.to_path_buf()
    };
    let made = match super::create::create(&typed) {
        Ok(made) => made,
        Err(e) => return RedoAttempt::refused(e),
    };
    let done = match Fingerprint::of(&made.path) {
        Ok(fingerprint) => Some(OpRecord::Create {
            path: made.path.clone(),
            is_dir: made.is_dir,
            fingerprint,
            created_parents: made.created_parents.clone(),
        }),
        Err(e) => {
            log::warn!("no undo record for {}: {e}", made.path.display());
            None
        }
    };
    // The parents first: the shallowest one made is the row a listing of the
    // directory above it shows.
    let mut touched = made.created_parents;
    touched.push(made.path);
    RedoAttempt {
        result: Ok(RedoReport {
            description: capitalised(&undone.record.describe_redo()),
            touched,
        }),
        done,
        remaining: None,
    }
}

/// The record a redo of `record` leaves on the undo stack for `links`: one
/// link stays a [`OpRecord::Link`], a gesture's worth stays one
/// [`OpRecord::Links`].
fn linked_as(record: &OpRecord, mut links: Vec<CreatedLink>) -> OpRecord {
    match record {
        OpRecord::Link { .. } if links.len() == 1 => {
            let one = links.remove(0);
            OpRecord::Link {
                link: one.link,
                target: one.target,
                original: one.original,
                fingerprint: one.fingerprint,
            }
        }
        _ => OpRecord::Links { links },
    }
}

/// Write every file's new tags again, all-or-nothing on the check.
///
/// [`undo_tags`] read forwards: each file must still carry exactly the tags
/// the undo put back — a set somebody has changed since is an edit this redo
/// knows nothing about — and one that does not refuses the whole redo before
/// any tag is written. A write that fails part way leaves what was written
/// undoable and the rest redoable.
fn redo_tags(undone: &Undone, changes: &[TagChange]) -> RedoAttempt {
    use crate::fs::tags;
    if changes.is_empty() {
        return RedoAttempt::refused(DfError::Op("nothing to redo".to_string()));
    }
    for change in changes {
        let now = match tags::try_read(&change.path) {
            Ok(now) if exists(&change.path) => now,
            _ => {
                return RedoAttempt::refused(DfError::Op(format!(
                    "cannot redo: {} is no longer there",
                    change.path.display()
                )))
            }
        };
        if now != change.before {
            return RedoAttempt::refused(DfError::Op(format!(
                "cannot redo: the tags of {} have changed since",
                change.path.display()
            )));
        }
    }
    let mut touched = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        if let Err(e) = tags::write(&change.path, &change.after) {
            return RedoAttempt {
                result: Err(DfError::Op(format!(
                    "cannot redo: {}: {e}",
                    change.path.display()
                ))),
                done: (i > 0).then(|| OpRecord::Tags {
                    changes: changes[..i].to_vec(),
                }),
                remaining: (i > 0).then(|| {
                    undone.rest(OpRecord::Tags {
                        changes: changes[i..].to_vec(),
                    })
                }),
            };
        }
        touched.push(change.path.clone());
    }
    RedoAttempt {
        result: Ok(RedoReport {
            description: capitalised(&undone.record.describe_redo()),
            touched,
        }),
        done: Some(undone.record.clone()),
        remaining: None,
    }
}

/// Make every link again: a symlink with the text it had, a hard link to the
/// file it was a second name for.
///
/// All-or-nothing on the check: every name free, and every hard link's
/// original still there, before the first link is made.
fn redo_links(undone: &Undone, links: &[CreatedLink], ctx: &TaskCtx) -> RedoAttempt {
    if links.is_empty() {
        return RedoAttempt::refused(DfError::Op("nothing to redo".to_string()));
    }
    for l in links {
        if let Err(e) = free(&l.link) {
            return RedoAttempt::refused(e);
        }
        match (&l.target, &l.original) {
            (Some(_), _) => {}
            (None, Some(original)) if exists(original) => {}
            (None, Some(original)) => {
                return RedoAttempt::refused(DfError::Op(format!(
                    "cannot redo: {} is no longer there to link to",
                    original.display()
                )))
            }
            (None, None) => {
                return RedoAttempt::refused(DfError::Op(format!(
                    "cannot redo: nothing records what {} was a hard link to",
                    l.link.display()
                )))
            }
        }
    }

    let mut done = Vec::new();
    let mut touched = Vec::new();
    for (i, l) in links.iter().enumerate() {
        let made =
            ctx.checkpoint()
                .and_then(|()| match (&l.target, &l.original) {
                    (Some(text), _) => crate::platform::fs::symlink(text, &l.link)
                        .map_err(|e| DfError::io(&l.link, e)),
                    (None, Some(original)) => super::link::hardlink(original, &l.link),
                    (None, None) => Err(DfError::Op(format!(
                        "cannot redo: nothing records what {} was a hard link to",
                        l.link.display()
                    ))),
                });
        if let Err(e) = made {
            return RedoAttempt {
                result: Err(e),
                done: (!done.is_empty()).then(|| linked_as(&undone.record, done)),
                remaining: (i > 0)
                    .then(|| undone.rest(linked_as(&undone.record, links[i..].to_vec()))),
            };
        }
        ctx.advance(0, 1);
        let again = match (&l.target, &l.original) {
            (None, Some(original)) => CreatedLink::record_hard(&l.link, original),
            _ => CreatedLink::record(&l.link, l.target.as_deref()),
        };
        match again {
            Ok(again) => done.push(again),
            Err(e) => log::warn!("no undo record for {}: {e}", l.link.display()),
        }
        touched.push(l.link.clone());
    }
    RedoAttempt {
        result: Ok(RedoReport {
            description: capitalised(&undone.record.describe_redo()),
            touched,
        }),
        done: (!done.is_empty()).then(|| linked_as(&undone.record, done)),
        remaining: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;
    use crate::ops::link::LinkKind;
    use crate::ops::{copy, create, link};

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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
    #[cfg(target_os = "linux")] // a trash to put things in (M2.8, W4.7)
    fn undo_of_a_trash_restores_it() {
        let t = TempTree::new("j-trash");
        let bin = crate::ops::Trash::at(t.join("Trash"));
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
    #[cfg(target_os = "linux")] // a trash to put things in (M2.8, W4.7)
    fn undo_of_a_trash_refuses_when_the_name_came_back() {
        let t = TempTree::new("j-trash-taken");
        let bin = crate::ops::Trash::at(t.join("Trash"));
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

    fn tag_list(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// `u` after `T` puts every file's old tags back — order and spelling as
    /// they were, and an untagged file untagged again — and says how many.
    #[test]
    fn undo_of_tags_restores_every_file() {
        use crate::fs::tags;
        let t = TempTree::new("undo-tags");
        let a = t.file("a.txt", b"a");
        if !tags::supported_here(&a) {
            return;
        }
        let b = t.file("b.txt", b"b");
        tags::write(&a, &tag_list(&["Work", "red"])).unwrap();
        let changes = vec![
            TagChange {
                path: a.clone(),
                before: tag_list(&["Work", "red"]),
                after: tag_list(&["red", "urgent"]),
            },
            TagChange {
                path: b.clone(),
                before: Vec::new(),
                after: tag_list(&["urgent"]),
            },
        ];
        for change in &changes {
            tags::write(&change.path, &change.after).unwrap();
        }
        let mut journal = Journal::default();
        journal.record(OpRecord::Tags { changes });
        assert_eq!(journal.peek().unwrap().describe(), "tagged 2 items");

        let report = journal.undo(&ctx()).unwrap();
        assert_eq!(report.description, "Restored tags of 2 items");
        assert_eq!(report.touched, vec![a.clone(), b.clone()]);
        assert_eq!(tags::read(&a), ["Work", "red"]);
        assert!(tags::read(&b).is_empty());
        assert!(journal.is_empty());

        let one = OpRecord::Tags {
            changes: vec![TagChange {
                path: a.clone(),
                before: Vec::new(),
                after: tag_list(&["Work", "red"]),
            }],
        };
        assert_eq!(
            undo_record(&one, &ctx()).unwrap().description,
            "Restored tags of a.txt"
        );
        assert!(tags::read(&a).is_empty());
    }

    /// A file whose tags changed after the prompt — another `T`, another
    /// program — refuses the whole undo before any file is touched, and the
    /// entry stays for when it can go through.
    #[test]
    fn undo_of_tags_refuses_a_set_that_has_changed() {
        use crate::fs::tags;
        let t = TempTree::new("undo-tags-changed");
        let a = t.file("a.txt", b"a");
        if !tags::supported_here(&a) {
            return;
        }
        let b = t.file("b.txt", b"b");
        let changes = vec![
            TagChange {
                path: a.clone(),
                before: Vec::new(),
                after: tag_list(&["red"]),
            },
            TagChange {
                path: b.clone(),
                before: Vec::new(),
                after: tag_list(&["red"]),
            },
        ];
        for change in &changes {
            tags::write(&change.path, &change.after).unwrap();
        }
        tags::write(&b, &tag_list(&["red", "blue"])).unwrap();
        let mut journal = Journal::default();
        journal.record(OpRecord::Tags { changes });

        let err = journal.undo(&ctx()).unwrap_err().to_string();
        assert!(err.contains("changed since"), "{err}");
        assert_eq!(
            tags::read(&a),
            ["red"],
            "not even the first file was touched"
        );
        assert_eq!(journal.len(), 1, "the entry waits");

        // Put back as the prompt left it, the undo goes through.
        tags::write(&b, &tag_list(&["red"])).unwrap();
        journal.undo(&ctx()).unwrap();
        assert!(tags::read(&a).is_empty() && tags::read(&b).is_empty());

        // A file that has gone is refused the same way.
        let gone = OpRecord::Tags {
            changes: vec![TagChange {
                path: t.join("gone.txt"),
                before: Vec::new(),
                after: tag_list(&["red"]),
            }],
        };
        let err = undo_record(&gone, &ctx()).unwrap_err().to_string();
        assert!(err.contains("no longer there"), "{err}");
    }

    /// `u`, `U`, `u` over a change of tags: each file gets the tags the
    /// prompt wrote again, the toast says so forwards, and `u` takes them
    /// off once more. A set changed between the undo and the redo refuses
    /// the whole redo, and the entry waits for the next `U`.
    #[test]
    fn redo_of_tags_round_trips_and_refuses_a_set_changed_since() {
        use crate::fs::tags;
        let t = TempTree::new("redo-tags");
        let a = t.file("a.txt", b"a");
        if !tags::supported_here(&a) {
            return;
        }
        let b = t.file("b.txt", b"b");
        tags::write(&a, &tag_list(&["Work"])).unwrap();
        let changes = vec![
            TagChange {
                path: a.clone(),
                before: tag_list(&["Work"]),
                after: tag_list(&["Work", "red"]),
            },
            TagChange {
                path: b.clone(),
                before: Vec::new(),
                after: tag_list(&["red"]),
            },
        ];
        for change in &changes {
            tags::write(&change.path, &change.after).unwrap();
        }
        let mut journal = Journal::default();
        journal.record(OpRecord::Tags { changes });
        journal.undo(&ctx()).unwrap();
        assert_eq!(tags::read(&a), ["Work"]);
        assert!(tags::read(&b).is_empty());
        assert_eq!(
            journal.peek_redo().unwrap().describe_redo(),
            "tagged 2 items again"
        );

        let report = redone(&mut journal);
        assert_eq!(report.description, "Tagged 2 items again");
        assert_eq!(report.touched, vec![a.clone(), b.clone()]);
        assert_eq!(tags::read(&a), ["Work", "red"]);
        assert_eq!(tags::read(&b), ["red"]);
        assert_eq!((journal.len(), journal.redo_len()), (1, 0));

        journal.undo(&ctx()).unwrap();
        assert_eq!(tags::read(&a), ["Work"]);
        assert!(tags::read(&b).is_empty());

        tags::write(&b, &tag_list(&["blue"])).unwrap();
        let err = journal.redo(&ctx()).unwrap_err().to_string();
        assert!(
            err.contains("b.txt") && err.contains("changed since"),
            "{err}"
        );
        assert_eq!(
            tags::read(&a),
            ["Work"],
            "not even the first file was touched"
        );
        assert_eq!(
            (journal.len(), journal.redo_len()),
            (0, 1),
            "the entry waits"
        );

        tags::write(&b, &[]).unwrap();
        redone(&mut journal);
        assert_eq!(tags::read(&b), ["red"]);
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
            original: None,
            fingerprint: Fingerprint::of(&l).unwrap(),
        });
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&l));
        assert!(target.is_file(), "the target is untouched");
    }

    #[cfg(unix)]
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
            original: None,
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
            original: None,
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
    #[cfg(unix)]
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

    // ── Redo ────────────────────────────────────────────────────────────────

    /// A fingerprint for a record whose paths are never touched: the
    /// sentences only.
    fn untouched() -> Fingerprint {
        Fingerprint {
            kind: FileKind::File,
            len: 0,
            mtime: None,
            entries: None,
        }
    }

    /// `U`, carried out as the app carries it out: done here, or — for a copy
    /// — the plan run the way a paste's job runs it, and the journal told what
    /// landed.
    fn redone(j: &mut Journal) -> RedoReport {
        match j.redo(&ctx()).unwrap() {
            Redo::Done(report) => report,
            Redo::Copy(copy) => {
                assert!(j.busy(), "held aside while it runs");
                let report = crate::ops::paste::execute(&copy.plan(), &ctx()).unwrap();
                j.finish_redo(report.record);
                RedoReport {
                    description: copy.description,
                    touched: copy.items.into_iter().map(|(_, dst)| dst).collect(),
                }
            }
        }
    }

    /// A rename of `from` to `to` in `t`, carried out and recorded.
    fn renamed(j: &mut Journal, t: &TempTree, from: &str, to: &str) -> (PathBuf, PathBuf) {
        let from = t.file(from, b"x");
        let to = t.join(to);
        create::rename(&from, &to, false).unwrap();
        j.record(OpRecord::Rename {
            moved: MovedPath::record(&from, &to).unwrap(),
        });
        (from, to)
    }

    /// `u`, `U`, `u`: the same step, back and forth, for as long as nothing
    /// else happens.
    #[test]
    fn redo_of_a_rename_round_trips() {
        let t = TempTree::new("j-redo-rename");
        let mut j = Journal::default();
        let (from, to) = renamed(&mut j, &t, "a.txt", "b.txt");

        j.undo(&ctx()).unwrap();
        assert!(from.is_file() && !super::exists(&to));
        assert_eq!((j.len(), j.redo_len()), (0, 1));

        let report = redone(&mut j);
        assert_eq!(report.description, "Renamed a.txt → b.txt again");
        assert_eq!(report.touched, vec![to.clone()]);
        assert!(to.is_file() && !super::exists(&from));
        assert_eq!((j.len(), j.redo_len()), (1, 0));
        assert!(
            matches!(j.peek(), Some(OpRecord::Rename { .. })),
            "still a rename, so the next toast still says renamed"
        );

        j.undo(&ctx()).unwrap();
        assert!(from.is_file() && !super::exists(&to), "and back again");
        assert!(j.can_redo());
    }

    #[test]
    fn redo_of_a_move_round_trips() {
        let t = TempTree::new("j-redo-move");
        let a = t.file("src/a.txt", b"a");
        let b = t.file("src/b.txt", b"b");
        t.dir("dst");
        let (a2, b2) = (t.join("dst/a.txt"), t.join("dst/b.txt"));
        copy::move_path(&a, &a2, &ctx(), false).unwrap();
        copy::move_path(&b, &b2, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Move {
            moves: vec![
                MovedPath::record(&a, &a2).unwrap(),
                MovedPath::record(&b, &b2).unwrap(),
            ],
        });

        j.undo(&ctx()).unwrap();
        assert!(a.is_file() && b.is_file());
        let report = redone(&mut j);
        assert_eq!(report.description, "Moved 2 items again");
        assert_eq!(std::fs::read(&a2).unwrap(), b"a");
        assert_eq!(std::fs::read(&b2).unwrap(), b"b");
        assert!(!super::exists(&a) && !super::exists(&b));

        j.undo(&ctx()).unwrap();
        assert!(a.is_file() && b.is_file() && !super::exists(&a2));
    }

    /// A bulk rename done again is still one gesture: one `U`, and one `u`
    /// to take it back.
    #[test]
    fn redo_of_a_bulk_rename_is_one_step_again() {
        let t = TempTree::new("j-redo-bulk");
        let mut moved = Vec::new();
        for (old, new) in [("a.txt", "1.txt"), ("b.txt", "2.txt"), ("c.txt", "3.txt")] {
            let from = t.file(old, b"x");
            let to = t.join(new);
            create::rename(&from, &to, false).unwrap();
            moved.push(MovedPath::record(&from, &to).unwrap());
        }
        let mut j = Journal::default();
        j.record(OpRecord::Renames { moved });

        j.undo(&ctx()).unwrap();
        let report = redone(&mut j);
        assert_eq!(report.description, "Renamed 3 items again");
        assert_eq!(j.len(), 1);
        match j.peek() {
            Some(OpRecord::Renames { moved }) => assert_eq!(moved.len(), 3),
            other => panic!("{other:?}"),
        }
        for name in ["1.txt", "2.txt", "3.txt"] {
            assert!(t.join(name).is_file(), "{name} was not renamed again");
        }
        j.undo(&ctx()).unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            assert!(t.join(name).is_file(), "{name} is not back");
        }
    }

    /// A redo of `d` puts the file back in the trash it was restored from,
    /// which is the one `u` will look in next.
    #[test]
    #[cfg(target_os = "linux")] // a trash to put things in (M2.8, W4.7)
    fn redo_of_a_trash_trashes_it_again_into_the_same_trash() {
        let t = TempTree::new("j-redo-trash");
        let bin = crate::ops::Trash::at(t.join("Trash"));
        let file = t.file("work/notes.txt", b"contents");
        let item = bin.trash(&file, &ctx()).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Trash { items: vec![item] });

        j.undo(&ctx()).unwrap();
        assert!(file.is_file());
        let report = redone(&mut j);
        assert_eq!(report.description, "Trashed notes.txt again");
        assert!(!super::exists(&file));
        match j.peek() {
            Some(OpRecord::Trash { items }) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].trash_root, t.join("Trash"));
                assert!(items[0].files_path().is_file());
            }
            other => panic!("{other:?}"),
        }

        j.undo(&ctx()).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"contents");
    }

    /// A redo of a paste copies the source again, through the copy a paste
    /// uses, and records the copy afresh, so `u` after it deletes exactly what
    /// it made.
    #[test]
    fn redo_of_a_copy_copies_it_again() {
        let t = TempTree::new("j-redo-copy");
        let src = t.dir("src");
        std::fs::write(src.join("a"), b"a").unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![CopyManifest::of_tree(&dst).unwrap().copied_from(&src)],
        });

        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&dst));
        let report = redone(&mut j);
        assert_eq!(report.description, "Copied dst again");
        assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"a");
        match j.peek() {
            Some(OpRecord::Copy { created }) => {
                assert_eq!(created[0].len(), 2, "dst and a");
                assert_eq!(
                    created[0].source().map(|s| s.path.clone()),
                    Some(src.clone())
                );
            }
            other => panic!("{other:?}"),
        }

        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&dst));
        assert_eq!(
            std::fs::read(src.join("a")).unwrap(),
            b"a",
            "the source stays"
        );
    }

    /// The source's fingerprint is checked before it is copied again: a file
    /// that has changed size since the paste would be copied as it is now,
    /// which is not what was undone.
    #[test]
    fn redo_of_a_copy_refuses_a_source_file_that_changed_size() {
        let t = TempTree::new("j-redo-copy-edited");
        let src = t.file("a.txt", b"one");
        let dst = t.join("b.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![CopyManifest::of_tree(&dst).unwrap().copied_from(&src)],
        });
        j.undo(&ctx()).unwrap();

        std::fs::write(&src, b"a different length").unwrap();
        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("cannot redo"), "{err}");
        assert!(!super::exists(&dst), "nothing was copied");
        assert_eq!(j.redo_len(), 1, "the entry stays for another try");
    }

    /// An extraction is recorded as a copy with no source, because what it
    /// copied was inside an archive: there is nothing to copy again.
    #[test]
    fn a_copy_with_no_source_cannot_be_redone() {
        let t = TempTree::new("j-redo-copy-sourceless");
        let src = t.file("a.txt", b"a");
        let dst = t.join("b.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(copied(&dst));
        j.undo(&ctx()).unwrap();

        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("nothing records where"), "{err}");
        assert!(!super::exists(&dst));
        assert_eq!(j.redo_len(), 1);
    }

    #[test]
    fn redo_of_a_create_makes_it_again_with_its_parents() {
        let t = TempTree::new("j-redo-create");
        let made = create::create(&t.join("a/b/notes.md")).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Create {
            path: made.path.clone(),
            is_dir: false,
            fingerprint: Fingerprint::of(&made.path).unwrap(),
            created_parents: made.created_parents.clone(),
        });

        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&t.join("a")));
        let report = redone(&mut j);
        assert_eq!(report.description, "Created file notes.md again");
        assert!(made.path.is_file());
        assert_eq!(
            report.touched.first(),
            Some(&t.join("a")),
            "the shallowest thing it made first, the row the listing shows"
        );
        match j.peek() {
            Some(OpRecord::Create {
                created_parents, ..
            }) => assert_eq!(created_parents, &made.created_parents),
            other => panic!("{other:?}"),
        }

        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&t.join("a")), "the parents go again too");
    }

    #[test]
    fn redo_of_a_created_folder_makes_the_folder() {
        let t = TempTree::new("j-redo-create-dir");
        let made = create::create(&t.join("folder/")).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Create {
            path: made.path.clone(),
            is_dir: true,
            fingerprint: Fingerprint::of(&made.path).unwrap(),
            created_parents: Vec::new(),
        });
        j.undo(&ctx()).unwrap();
        let report = redone(&mut j);
        assert_eq!(report.description, "Created folder folder again");
        assert!(made.path.is_dir());
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&made.path));
    }

    /// An archive, or the system clipboard pasted into a file, is recorded as
    /// a create so that `u` can take it away — and nothing holds what was
    /// written into it, so `U` says so rather than making an empty one.
    #[test]
    fn a_create_that_was_written_cannot_be_redone() {
        let t = TempTree::new("j-redo-create-written");
        let path = t.file("photos.zip", b"archive bytes");
        let mut j = Journal::default();
        j.record(OpRecord::Create {
            path: path.clone(),
            is_dir: false,
            fingerprint: Fingerprint::of(&path).unwrap(),
            created_parents: Vec::new(),
        });
        j.undo(&ctx()).unwrap();

        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("only an empty one"), "{err}");
        assert!(!super::exists(&path), "no empty stand-in was made");
        assert_eq!(j.redo_len(), 1);
    }

    #[test]
    fn redo_of_a_symlink_links_it_again_with_the_same_text() {
        let t = TempTree::new("j-redo-symlink");
        let target = t.file("target.txt", b"x");
        let l = t.join("link");
        let text = link::symlink(&target, &l, LinkKind::Relative).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Link {
            link: l.clone(),
            target: Some(text.clone()),
            original: None,
            fingerprint: Fingerprint::of(&l).unwrap(),
        });

        j.undo(&ctx()).unwrap();
        let report = redone(&mut j);
        assert_eq!(report.description, "Linked link again");
        assert_eq!(std::fs::read_link(&l).unwrap(), text);
        assert!(matches!(j.peek(), Some(OpRecord::Link { .. })));
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&l));
        assert!(target.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn redo_of_a_hard_link_links_the_same_file_again() {
        let t = TempTree::new("j-redo-hardlink");
        let target = t.file("target.txt", b"x");
        let hard = t.join("hard");
        link::hardlink(&target, &hard).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Link {
            link: hard.clone(),
            target: None,
            original: Some(target.clone()),
            fingerprint: Fingerprint::of(&hard).unwrap(),
        });

        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&hard));
        redone(&mut j);
        assert_eq!(
            crate::platform::meta::ino(&std::fs::metadata(&hard).unwrap()),
            crate::platform::meta::ino(&std::fs::metadata(&target).unwrap()),
            "a second name for the same file"
        );
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&hard));
    }

    #[test]
    fn redo_of_a_batch_of_links_is_one_step_again() {
        let t = TempTree::new("j-redo-links");
        let mut links = Vec::new();
        for name in ["a.txt", "ünïcödé — 日本語 🎬.txt"] {
            let target = t.file(name, b"x");
            let l = t.join(format!("link-{name}"));
            let text = link::symlink(&target, &l, LinkKind::Absolute).unwrap();
            links.push(CreatedLink::record(&l, Some(&text)).unwrap());
        }
        let original = t.file("hard-target", b"x");
        let hard = t.join("hard-link");
        link::hardlink(&original, &hard).unwrap();
        links.push(CreatedLink::record_hard(&hard, &original).unwrap());
        let mut j = Journal::default();
        j.record(OpRecord::Links {
            links: links.clone(),
        });

        j.undo(&ctx()).unwrap();
        let report = redone(&mut j);
        assert_eq!(report.description, "Linked 3 items again");
        for l in &links {
            assert!(super::exists(&l.link), "{}", l.link.display());
        }
        assert_eq!(j.len(), 1, "one gesture, one entry");
        j.undo(&ctx()).unwrap();
        for l in &links {
            assert!(!super::exists(&l.link), "{}", l.link.display());
        }
    }

    /// A hard link recorded without its original has nothing to be linked to
    /// again, and the whole batch refuses before any of it is made.
    #[test]
    fn a_hard_link_with_no_original_cannot_be_redone() {
        let t = TempTree::new("j-redo-hardlink-unknown");
        let original = t.file("target.txt", b"x");
        let hard = t.join("hard");
        link::hardlink(&original, &hard).unwrap();
        let sym = t.join("sym");
        let text = link::symlink(&original, &sym, LinkKind::Absolute).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Links {
            links: vec![
                CreatedLink::record(&sym, Some(&text)).unwrap(),
                CreatedLink::record(&hard, None).unwrap(),
            ],
        });
        j.undo(&ctx()).unwrap();

        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("hard link to"), "{err}");
        assert!(!super::exists(&sym), "nothing was made");
    }

    /// Something new happened, so what was undone before it cannot be redone
    /// on top of it.
    #[test]
    fn a_new_record_clears_the_redo_stack() {
        let t = TempTree::new("j-redo-cleared");
        let mut j = Journal::default();
        renamed(&mut j, &t, "a.txt", "b.txt");
        j.undo(&ctx()).unwrap();
        assert!(j.can_redo());

        renamed(&mut j, &t, "c.txt", "d.txt");
        assert!(!j.can_redo());
        assert_eq!(j.peek_redo(), None);
        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("nothing to redo"), "{err}");
        assert!(t.join("a.txt").is_file(), "the undone rename stayed undone");
    }

    /// An undo that stopped half way has no forward form for the half that
    /// went, so the line of redos behind it goes with it. One that was
    /// refused outright changed nothing, and leaves the line alone.
    #[cfg(unix)]
    #[test]
    fn a_partial_undo_clears_the_redo_stack_and_a_refused_one_does_not() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new("j-redo-partial");
        let a = t.file("one/a.txt", b"a");
        let b = t.file("two/b.txt", b"b");
        let dest = t.dir("dst");
        let (a2, b2) = (dest.join("a.txt"), dest.join("b.txt"));
        copy::move_path(&a, &a2, &ctx(), false).unwrap();
        copy::move_path(&b, &b2, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Move {
            moves: vec![
                MovedPath::record(&a, &a2).unwrap(),
                MovedPath::record(&b, &b2).unwrap(),
            ],
        });
        renamed(&mut j, &t, "c.txt", "d.txt");
        j.undo(&ctx()).unwrap();
        assert!(j.can_redo());

        // Refused outright: the way back to one/ is taken. Nothing moved.
        std::fs::write(&a, b"newer").unwrap();
        assert!(j.undo(&ctx()).is_err());
        assert!(j.can_redo(), "a refusal changes nothing");
        std::fs::remove_file(&a).unwrap();

        // Half way: a.txt goes back, b.txt cannot.
        std::fs::set_permissions(t.join("two"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let partial = j.undo(&ctx());
        std::fs::set_permissions(t.join("two"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(partial.is_err());
        assert!(a.is_file(), "the first leg went back");
        assert!(!j.can_redo(), "the redo line went with it");
        assert_eq!(j.len(), 1, "the remainder is still there to undo");
    }

    /// The world moved between the undo and the redo: the file the undo put
    /// back has been edited since, and moving it again would carry the edit
    /// somewhere the user did not ask it to go. Refused, untouched, kept.
    #[test]
    fn redo_refuses_a_file_edited_between_the_undo_and_the_redo() {
        let t = TempTree::new("j-redo-edited");
        let mut j = Journal::default();
        let (from, to) = renamed(&mut j, &t, "a.txt", "b.txt");
        j.undo(&ctx()).unwrap();

        std::fs::write(&from, b"edited after the undo").unwrap();
        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().starts_with("cannot redo"), "{err}");
        assert!(err.to_string().contains("changed size"), "{err}");
        assert_eq!(std::fs::read(&from).unwrap(), b"edited after the undo");
        assert!(!super::exists(&to), "nothing was moved");
        assert_eq!((j.len(), j.redo_len()), (0, 1), "the entry stays");
    }

    #[test]
    fn redo_refuses_a_name_taken_since() {
        let t = TempTree::new("j-redo-taken");
        let mut j = Journal::default();
        let (from, to) = renamed(&mut j, &t, "a.txt", "b.txt");
        j.undo(&ctx()).unwrap();

        std::fs::write(&to, b"somebody else's b.txt").unwrap();
        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("taken now"), "{err}");
        assert!(from.is_file());
        assert_eq!(std::fs::read(&to).unwrap(), b"somebody else's b.txt");
    }

    /// A copy is not made by `U` itself: it is checked, handed back as a
    /// paste's plan, and held aside — `u` and `U` waiting — until the caller
    /// says what landed. Nothing landing puts it back to be redone.
    #[test]
    fn a_copy_is_redone_as_a_plan_the_caller_runs() {
        let t = TempTree::new("j-redo-copy-plan");
        let src = t.file("a.txt", b"a");
        let dst = t.join("b.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![CopyManifest::of_tree(&dst).unwrap().copied_from(&src)],
        });
        j.undo(&ctx()).unwrap();

        let Redo::Copy(copy) = j.redo(&ctx()).unwrap() else {
            panic!("a copy is handed back");
        };
        assert_eq!(copy.items, vec![(src.clone(), dst.clone())]);
        assert_eq!(copy.description, "Copied b.txt again");
        let plan = copy.plan();
        assert_eq!(plan.mode, PasteMode::Copy);
        assert_eq!(Some(plan.dest_dir.as_path()), dst.parent());
        assert!(plan.is_settled() && plan.ready.iter().all(|item| !item.overwrite));
        assert!(!super::exists(&dst), "nothing copied yet");
        assert!(j.busy());
        assert_eq!((j.len(), j.redo_len()), (0, 0), "held aside");
        for err in [j.undo(&ctx()).unwrap_err(), j.redo(&ctx()).unwrap_err()] {
            assert!(err.to_string().contains("still running"), "{err}");
        }

        // Cancelled before it ran: back on the redo stack, whole.
        j.finish_redo(None);
        assert!(!j.busy());
        assert_eq!((j.len(), j.redo_len()), (0, 1));

        // Run this time: on the undo stack, and `u` takes it back.
        redone(&mut j);
        assert_eq!(std::fs::read(&dst).unwrap(), b"a");
        assert_eq!((j.len(), j.redo_len()), (1, 0));
        j.undo(&ctx()).unwrap();
        assert!(!super::exists(&dst));
        assert!(j.can_redo());
    }

    /// A copy that lands in part — cancelled after its first item — puts that
    /// item on the undo stack and the rest back on the redo stack.
    #[test]
    fn a_copy_redo_that_lands_in_part_splits_across_the_stacks() {
        let t = TempTree::new("j-redo-copy-part");
        let a = t.file("src/a.txt", b"a");
        let b = t.file("src/b.txt", b"b");
        let dest = t.dir("dst");
        let (a2, b2) = (dest.join("a.txt"), dest.join("b.txt"));
        copy::copy_tree(&a, &a2, &ctx(), false).unwrap();
        copy::copy_tree(&b, &b2, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![
                CopyManifest::of_tree(&a2).unwrap().copied_from(&a),
                CopyManifest::of_tree(&b2).unwrap().copied_from(&b),
            ],
        });
        j.undo(&ctx()).unwrap();

        let Redo::Copy(_) = j.redo(&ctx()).unwrap() else {
            panic!("a copy is handed back");
        };
        // The job copied `a.txt` and was cancelled before `b.txt`.
        copy::copy_tree(&a, &a2, &ctx(), false).unwrap();
        j.finish_redo(Some(OpRecord::Copy {
            created: vec![CopyManifest::of_tree(&a2).unwrap().copied_from(&a)],
        }));
        match (j.peek(), j.peek_redo()) {
            (Some(OpRecord::Copy { created: done }), Some(OpRecord::Copy { created: rest })) => {
                assert_eq!(done[0].root, a2);
                assert_eq!(rest.len(), 1);
                assert_eq!(rest[0].root, b2);
            }
            other => panic!("{other:?}"),
        }
        redone(&mut j);
        assert_eq!(std::fs::read(&b2).unwrap(), b"b", "the rest, redone");
    }

    /// Something new recorded while a redo's copy runs clears the line it came
    /// off: what does not land is not put back on a stack that is gone.
    #[test]
    fn a_copy_redo_that_fails_after_something_new_is_not_put_back() {
        let t = TempTree::new("j-redo-copy-orphan");
        let src = t.file("a.txt", b"a");
        let dst = t.join("b.txt");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![CopyManifest::of_tree(&dst).unwrap().copied_from(&src)],
        });
        j.undo(&ctx()).unwrap();
        let Redo::Copy(_) = j.redo(&ctx()).unwrap() else {
            panic!("a copy is handed back");
        };
        renamed(&mut j, &t, "c.txt", "d.txt");
        j.finish_redo(None);
        assert!(!j.busy());
        assert!(!j.can_redo(), "the line went with the new record");
        assert_eq!(j.len(), 1, "only the rename");
    }

    /// The source check is the source's own fingerprint and no deeper: a file
    /// changed inside a copied folder is not seen, and the folder is copied
    /// again as it is now ([`CopySource`]).
    #[test]
    fn a_copy_redo_copies_a_folder_as_it_is_now() {
        let t = TempTree::new("j-redo-copy-deep");
        let src = t.dir("src");
        std::fs::write(src.join("notes.txt"), b"before").unwrap();
        let dst = t.join("dst");
        copy::copy_tree(&src, &dst, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Copy {
            created: vec![CopyManifest::of_tree(&dst).unwrap().copied_from(&src)],
        });
        j.undo(&ctx()).unwrap();

        std::fs::write(src.join("notes.txt"), b"after, and longer").unwrap();
        redone(&mut j);
        assert_eq!(
            std::fs::read(dst.join("notes.txt")).unwrap(),
            b"after, and longer"
        );
    }

    /// A trash undo that fails part way hands back what is still in the trash,
    /// as a move's does: the entry stops listing the file that went back, a
    /// second `u` finishes, and the redo line — which has no forward form for
    /// the half that went — is cleared.
    #[cfg(unix)]
    #[test]
    #[cfg(target_os = "linux")] // a trash to put things in (M2.8, W4.7)
    fn a_partly_undone_trash_leaves_only_the_remainder_and_clears_the_redo_stack() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new("j-trash-partial");
        let bin = crate::ops::Trash::at(t.join("Trash"));
        let a = t.file("one/a.txt", b"a");
        let b = t.file("two/b.txt", b"b");
        let items = vec![
            bin.trash(&a, &ctx()).unwrap(),
            bin.trash(&b, &ctx()).unwrap(),
        ];
        let mut j = Journal::default();
        j.record(OpRecord::Trash { items });
        renamed(&mut j, &t, "c.txt", "d.txt");
        j.undo(&ctx()).unwrap();
        assert!(j.can_redo());

        // `two/` cannot be written to, so `b.txt` cannot go back into it —
        // but `a.txt` already has.
        std::fs::set_permissions(t.join("two"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let partial = j.undo(&ctx());
        std::fs::set_permissions(t.join("two"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(partial.is_err());
        assert!(a.is_file(), "the first went back");
        assert!(!super::exists(&b), "the second did not");
        assert!(!j.can_redo(), "the redo line went with it");
        match j.peek() {
            Some(OpRecord::Trash { items }) => {
                assert_eq!(items.len(), 1, "only what is still in the trash");
                assert_eq!(items[0].original, b);
            }
            other => panic!("{other:?}"),
        }

        j.undo(&ctx()).unwrap();
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        assert!(j.is_empty());
    }

    /// A redo that fails part way is split across the stacks: what moved is on
    /// the undo stack, what did not is on the redo stack — and `u`, `U`, `U`
    /// walks both halves.
    #[cfg(unix)]
    #[test]
    fn a_partial_redo_splits_and_both_halves_can_be_walked() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new("j-redo-split");
        let a = t.file("one/a.txt", b"a");
        let b = t.file("two/b.txt", b"b");
        let (a2, b2) = (t.dir("dst1").join("a.txt"), t.dir("dst2").join("b.txt"));
        copy::move_path(&a, &a2, &ctx(), false).unwrap();
        copy::move_path(&b, &b2, &ctx(), false).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Move {
            moves: vec![
                MovedPath::record(&a, &a2).unwrap(),
                MovedPath::record(&b, &b2).unwrap(),
            ],
        });
        j.undo(&ctx()).unwrap();

        // `dst2/` cannot be written to: `a.txt` moves again, `b.txt` cannot.
        std::fs::set_permissions(t.join("dst2"), std::fs::Permissions::from_mode(0o555)).unwrap();
        let partial = j.redo(&ctx());
        std::fs::set_permissions(t.join("dst2"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(partial.is_err());
        assert!(a2.is_file() && b.is_file());
        match (j.peek(), j.peek_redo()) {
            (Some(OpRecord::Move { moves: done }), Some(OpRecord::Move { moves: rest })) => {
                assert_eq!(done.len(), 1);
                assert_eq!(done[0].to, a2, "what moved can be undone");
                assert_eq!(rest.len(), 1);
                assert_eq!(rest[0].to, b2, "what did not can be redone");
            }
            other => panic!("{other:?}"),
        }

        // `u` takes the half that moved back; `U` does it again; `U` does the
        // other half.
        j.undo(&ctx()).unwrap();
        assert!(a.is_file() && !super::exists(&a2));
        assert_eq!(j.redo_len(), 2);
        redone(&mut j);
        assert!(a2.is_file());
        redone(&mut j);
        assert!(b2.is_file() && !super::exists(&b));
        assert_eq!((j.len(), j.redo_len()), (2, 0));
    }

    /// The name a redo of `d` would trash again now belongs to a different
    /// file: refused, and the new file stays where it is.
    #[test]
    #[cfg(target_os = "linux")] // a trash to put things in (M2.8, W4.7)
    fn redo_of_a_trash_refuses_a_file_replaced_under_the_same_name() {
        let t = TempTree::new("j-redo-trash-replaced");
        let bin = crate::ops::Trash::at(t.join("Trash"));
        let file = t.file("work/notes.txt", b"the old notes");
        let item = bin.trash(&file, &ctx()).unwrap();
        let mut j = Journal::default();
        j.record(OpRecord::Trash { items: vec![item] });
        j.undo(&ctx()).unwrap();

        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, b"somebody else's notes, longer").unwrap();
        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().starts_with("cannot redo"), "{err}");
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"somebody else's notes, longer"
        );
        assert_eq!(j.redo_len(), 1, "the entry stays");
        assert!(j.is_empty());
    }

    #[test]
    fn redo_with_nothing_undone_says_so() {
        let mut j = Journal::default();
        let err = j.redo(&ctx()).unwrap_err();
        assert!(err.to_string().contains("nothing to redo"), "{err}");
        assert!(!j.can_redo());
    }

    /// Both sides of the history newest first, and a stamp that is the
    /// operation's own wherever the entry is: undone and redone, it happened
    /// when it happened.
    #[test]
    fn the_history_reads_newest_first_and_keeps_each_entrys_time() {
        let t = TempTree::new("j-history");
        let t0 = Instant::now();
        let mut j = Journal::default();
        let mut paths = Vec::new();
        for (i, name) in ["one", "two", "three"].iter().enumerate() {
            let made = create::create(&t.join(name)).unwrap();
            j.record_at(
                OpRecord::Create {
                    path: made.path.clone(),
                    is_dir: false,
                    fingerprint: Fingerprint::of(&made.path).unwrap(),
                    created_parents: Vec::new(),
                },
                t0 + std::time::Duration::from_secs(i as u64),
            );
            paths.push(made.path);
        }
        let names = |rows: Vec<(&OpRecord, Instant)>| -> Vec<(String, Instant)> {
            rows.into_iter()
                .map(|(record, at)| (record.describe(), at))
                .collect()
        };
        let secs = |n: u64| t0 + std::time::Duration::from_secs(n);

        j.undo(&ctx()).unwrap();
        j.undo(&ctx()).unwrap();
        assert_eq!(
            names(j.undoable().collect()),
            vec![("created file one".to_string(), secs(0))]
        );
        assert_eq!(
            names(j.redoable().collect()),
            vec![
                ("created file two".to_string(), secs(1)),
                ("created file three".to_string(), secs(2)),
            ],
            "the next redo first"
        );

        redone(&mut j);
        assert_eq!(
            names(j.undoable().collect()),
            vec![
                ("created file two".to_string(), secs(1)),
                ("created file one".to_string(), secs(0)),
            ]
        );
        assert_eq!(
            j.peek_redo().map(OpRecord::describe_redo).as_deref(),
            Some("created file three again")
        );
    }

    #[test]
    fn records_describe_what_a_redo_does() {
        let leg = |from: &str, to: &str| MovedPath {
            from: PathBuf::from(from),
            to: PathBuf::from(to),
            fingerprint: untouched(),
        };
        assert_eq!(
            OpRecord::Rename {
                moved: leg("/w/a.txt", "/w/b.txt")
            }
            .describe_redo(),
            "renamed a.txt → b.txt again"
        );
        assert_eq!(
            OpRecord::Move {
                moves: vec![leg("/w/a.txt", "/x/a.txt")]
            }
            .describe_redo(),
            "moved a.txt again"
        );
        assert_eq!(
            OpRecord::Renames {
                moved: vec![leg("/w/a", "/w/b"); 4]
            }
            .describe_redo(),
            "renamed 4 items again"
        );
        assert_eq!(
            OpRecord::Create {
                path: PathBuf::from("/w/docs"),
                is_dir: true,
                fingerprint: untouched(),
                created_parents: Vec::new(),
            }
            .describe_redo(),
            "created folder docs again"
        );
        assert_eq!(
            OpRecord::Copy { created: vec![] }.describe_redo(),
            "copied 0 items again"
        );
        assert_eq!(
            OpRecord::Link {
                link: PathBuf::from("/w/l"),
                target: None,
                original: Some(PathBuf::from("/w/t")),
                fingerprint: untouched(),
            }
            .describe_redo(),
            "linked l again"
        );
    }
}
