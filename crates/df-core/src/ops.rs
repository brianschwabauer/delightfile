//! File operations and the op journal that makes them reversible.
//!
//! Every mutation — move, rename, copy, trash, symlink, create, a change of
//! permissions — records its inverse before it runs, and `u` replays it
//! backwards (PLAN §5). Trash goes
//! through the freedesktop trash spec, hand-rolled, precisely so that `d` is
//! always undoable; permanent delete is the one operation with no inverse, and
//! it is the only one that gets a confirm dialog. `U` walks forward again: an
//! undo keeps what it took back, and a redo repeats it after checking the
//! world the way an undo does ([`journal`]'s header). Copies are reflink-first
//! (`FICLONE`) with a read/write fallback, so duplicating a 40 GB file on btrfs
//! is instant and on ext4 is merely a copy.
//!
//! ## The shape of an operation
//!
//! Every operation in here is a plain function taking a [`TaskCtx`](crate::tasks::TaskCtx).
//! That is the whole cancellation and progress story: the function calls
//! `ctx.checkpoint()?` between chunks and directory entries, and `ctx.advance()`
//! as bytes and files land. Run it on the pool and it is a task with a bar in
//! the `w` panel; run it with `TaskCtx::detached()` and it is a synchronous
//! call in a test. No operation knows which it is.
//!
//! ## What is deliberately not here (v1)
//!
//! - **Hardlink preservation across a copy.** Copying a tree in which two files
//!   are the same inode produces two independent files. Preserving the link
//!   means carrying an inode map for the whole tree and is only correct if the
//!   copy is one atomic operation; `cp -a` does it, and so will v2. Nothing is
//!   lost by not doing it, only disk.
//! - **ACLs, ownership, and every extended attribute outside `user.*`.**
//!   `chown` needs privileges we do not have, and `security.*`, `trusted.*`
//!   and the ACLs in `system.*` belong to the kernel and to root. Mode and
//!   mtime are preserved, which is what a file manager's user sees — and so
//!   are the `user.*` attributes, the file's tags among them
//!   ([`crate::fs::tags`]): every copy of bytes (a paste, a move or a trash
//!   across drives, a sync) carries them after the contents and before the
//!   rename into place. A destination that holds none, a FAT card, gets the
//!   file without them and the paste says so; a same-drive rename keeps them
//!   for nothing, since it keeps the inode. The archive writers do not store
//!   them (see [`crate::archive::write`]).
//! - **Queue reordering** in the task engine — that is the `w` panel's half of
//!   the phase.

pub mod copy;
pub mod create;
pub mod delete;
pub mod jobs;
pub mod journal;
pub mod link;
pub mod mode;
pub mod paste;
pub mod trash;

use std::path::{Component, Path, PathBuf};

use crate::{DfError, Result};

pub use copy::{
    copy_tree, copy_tree_with, measure, move_cross_device, move_path, CopyOptions, CopyStats,
    COPY_CHUNK,
};
pub use create::{create, rename, Created};
pub use delete::{check_deletable, check_deletable_here, delete_permanent, remove_tree};
pub use jobs::{DeleteJob, ExtractJob, ModeJob, OpOutcome, Outcome, PasteJob, TrashJob};
pub use journal::{
    undo_attempt, undo_record, CopyManifest, CopySource, CreatedLink, FileKind, Fingerprint,
    Journal, MovedPath, OpRecord, Redo, RedoCopy, RedoReport, TagChange, UndoAttempt, UndoReport,
    Undone, JOURNAL_DEPTH, MAX_MANIFEST_ENTRIES,
};
pub use link::{hardlink, relative_to, symlink, LinkKind};
pub use paste::{
    execute as paste, plan_paste, unique_name, Clipboard, Conflict, PasteItem, PasteMode,
    PastePlan, PasteReport, Resolution, Toggled,
};
pub use trash::{purge, Trash, TrashedItem};

/// Make a path absolute and lexically clean, without touching the disk.
///
/// Lexical on purpose: `canonicalize` resolves symlinks, and a file manager
/// that silently rewrote `~/Downloads/link-to-huge` into the huge thing's real
/// path would copy, move and delete the wrong object. `..` is popped
/// lexically, which is wrong in the presence of symlinked parents — so it is
/// only ever used for *comparisons and display*, never as the path an
/// operation actually acts on. Operations use the path the caller gave them.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    for comp in absolute.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push("/");
    }
    out
}

/// Is this a URL — `sftp://host/photos`, `trash://` — rather than a path?
///
/// The panes address places that are not on this machine by URL, carried in a
/// `PathBuf` like any other row's path. Such a path is *relative* as far as
/// `Path` can tell, so [`normalize`] would put the process's working directory
/// in front of it: `sftp://host/photos` became `<cwd>/sftp:/host/photos`, a
/// local path that does not exist. A URL is one when its first component is a
/// scheme — a letter, then letters, digits, `+`, `-` or `.` — followed by
/// `://`, which no path a person types into a file manager starts with.
pub fn is_url(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    let Some(colon) = bytes.windows(3).position(|w| w == b"://") else {
        return false;
    };
    let scheme = &bytes[..colon];
    scheme.first().is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// Is `ancestor` at or above `path` in the tree? Lexical, over normalized
/// paths, so `/a/b` is an ancestor of `/a/b/c` but not of `/a/bc`.
pub fn is_ancestor(ancestor: &Path, path: &Path) -> bool {
    let a = normalize(ancestor);
    let p = normalize(path);
    p.starts_with(&a)
}

/// Strictly below: an ancestor that is not the path itself.
pub fn is_strict_ancestor(ancestor: &Path, path: &Path) -> bool {
    let a = normalize(ancestor);
    let p = normalize(path);
    p != a && p.starts_with(&a)
}

/// A path with every symlink in it resolved — including a path that is not
/// there yet, whose parent is resolved and whose name is put back.
///
/// `None` when even the parent cannot be resolved, which leaves the caller with
/// nothing better than the lexical answer.
fn resolved(path: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::fs::canonicalize(path) {
        return Some(p);
    }
    let parent = path.parent()?;
    let name = path.file_name()?;
    Some(std::fs::canonicalize(parent).ok()?.join(name))
}

/// [`is_ancestor`], asking the filesystem instead of the text.
///
/// The lexical answer is the right one for display and for `..`, and the wrong
/// one for the "into itself" rails: `~/link` and `~/project` are one directory
/// when `link` points at it, and copying `project` into `link` recurses until
/// the disk is full. Falls back to the lexical answer when either side cannot
/// be resolved, so this never says "safe" where [`is_ancestor`] said "unsafe".
pub fn is_ancestor_resolved(ancestor: &Path, path: &Path) -> bool {
    if is_ancestor(ancestor, path) {
        return true;
    }
    matches!(
        (resolved(ancestor), resolved(path)),
        (Some(a), Some(p)) if p.starts_with(&a)
    )
}

/// [`is_strict_ancestor`], resolved. See [`is_ancestor_resolved`].
pub fn is_strict_ancestor_resolved(ancestor: &Path, path: &Path) -> bool {
    if is_strict_ancestor(ancestor, path) {
        return true;
    }
    matches!(
        (resolved(ancestor), resolved(path)),
        (Some(a), Some(p)) if p != a && p.starts_with(&a)
    )
}

/// Is this a directory *itself*, rather than a symlink to one?
///
/// The recursion rails only apply to a real directory: copying a symlink that
/// points at an ancestor recreates one link and recurses into nothing.
pub fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

/// A path with any trailing `/` trimmed, for the calls that must act on the
/// *link* rather than on what it points at.
///
/// POSIX makes a trailing slash mean "and this had better be a directory", so
/// `lstat("link/")` follows the final symlink and answers about the target.
/// A recursive delete handed `~/link/` would therefore descend into the target
/// and empty it. Everything that unlinks trims first.
pub fn trim_trailing_slash(path: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let raw = path.as_os_str().as_bytes();
    let mut end = raw.len();
    while end > 1 && raw[end - 1] == b'/' {
        end -= 1;
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&raw[..end]))
}

/// Are these two paths the same file *on disk* (same device and inode)?
///
/// The lexical comparison is not enough: `~/x` and `/home/brian/x` are the same
/// file through a symlinked `~`, and the "do not overwrite the source with its
/// own copy" rail has to catch that. `false` when either side cannot be
/// stat'ed, since a path that does not exist is not the same file as anything.
pub fn same_file(a: &Path, b: &Path) -> bool {
    crate::platform::fs::same_file(a, b).unwrap_or(false)
}

/// The file name of a path, or an error naming the path that has none
/// (`/`, or something ending in `..`).
pub fn file_name(path: &Path) -> Result<&std::ffi::OsStr> {
    path.file_name()
        .ok_or_else(|| DfError::Op(format!("{} has no file name", path.display())))
}

/// Does this path exist, counting a broken symlink as existing?
///
/// `Path::exists` follows the link and answers "no" for a broken one, which
/// would have us happily clobber it.
pub fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The operation fixtures, re-exported under the name the tests grew up with.
///
/// The fixture itself moved to [`crate::test_support`] so df-app's tests can
/// build against the same `TempTree` instead of growing a second one that
/// drifts (PLAN §9 wants *one* set of gnarly names, exercised everywhere).
#[cfg(test)]
pub(crate) use crate::test_support as fixture;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    #[test]
    fn normalize_cleans_lexically() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), Path::new("/a/c"));
        assert_eq!(normalize(Path::new("/a/b/../..")), Path::new("/"));
        assert_eq!(normalize(Path::new("/")), Path::new("/"));
        assert_eq!(normalize(Path::new("/a//b")), Path::new("/a/b"));
    }

    #[test]
    fn normalize_makes_relative_absolute() {
        let n = normalize(Path::new("relative/thing"));
        assert!(n.is_absolute(), "{}", n.display());
        assert!(n.ends_with("relative/thing"));
    }

    #[test]
    fn a_url_is_told_from_a_path() {
        assert!(is_url(Path::new("sftp://showandtour1/photos")));
        assert!(is_url(Path::new("sftp://showandtour1")));
        assert!(is_url(Path::new("trash://")));
        assert!(
            !is_url(Path::new("/home/brian/sftp://x")),
            "not a scheme first"
        );
        assert!(!is_url(Path::new("relative/dir")));
        assert!(
            !is_url(Path::new("2024://odd")),
            "a scheme starts with a letter"
        );
        assert!(!is_url(Path::new("://nothing")));
    }

    #[test]
    fn ancestry_is_component_wise() {
        assert!(is_ancestor(Path::new("/a/b"), Path::new("/a/b/c")));
        assert!(is_ancestor(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!is_strict_ancestor(Path::new("/a/b"), Path::new("/a/b")));
        assert!(
            !is_ancestor(Path::new("/a/b"), Path::new("/a/bc")),
            "a prefix of the *name* is not an ancestor"
        );
    }

    #[test]
    fn same_file_sees_through_a_symlinked_route() {
        let t = fixture::TempTree::new("samefile");
        let real = t.file("dir/file.txt", b"hi");
        t.symlink(t.join("dir"), "link");
        let through_link = t.join("link/file.txt");
        assert!(same_file(&real, &through_link));
        assert!(!same_file(&real, &t.join("dir/other")));
    }

    #[test]
    fn exists_counts_a_broken_symlink() {
        let t = fixture::TempTree::new("broken");
        let link = t.symlink(t.join("nowhere"), "broken");
        assert!(!link.exists(), "std follows the link");
        assert!(exists(&link), "but the link itself is there");
    }
}
