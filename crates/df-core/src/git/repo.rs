//! Where the repository is, and what `HEAD` says — without running `git`.
//!
//! Three file reads, no subprocess. Everything here is on the hot path of
//! entering a directory, so everything here is cheap and synchronous; the
//! expensive question (status) lives in [`super::cache`] and is not.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// How many levels to walk up looking for a `.git`.
///
/// Paths deeper than this exist (`node_modules` nests like it is being paid to)
/// but a repository root sixty-four levels above the cursor does not. The cap is
/// here so that a pathological path — a symlink loop resolved into a
/// thousand-component name, say — cannot turn "which repo is this" into a
/// thousand `stat` calls on every keystroke.
pub const MAX_WALK_DEPTH: usize = 64;

/// The most of a `.git` *file* to read: 4 KiB.
///
/// A gitfile is one line — `gitdir: /path/to/.git/worktrees/name` — and a path
/// cannot exceed `PATH_MAX`, so 4 KiB is already generous. It exists because
/// `.git` being a file at all is a surprise, and a `.git` that is a 2 GB file is
/// a surprise this code should survive rather than `read_to_string`.
pub const MAX_GITFILE_BYTES: u64 = 4096;

/// The most of `HEAD` to read: 4 KiB.
///
/// `HEAD` is either 41 bytes of hash or `ref: ` plus a ref name. Same argument
/// as above: the cap costs nothing and removes a whole class of bad day.
pub const MAX_HEAD_BYTES: u64 = 4096;

/// How much of a detached hash to show: 7 characters.
///
/// git's own default abbreviation, and the length everyone reads as "a commit"
/// rather than "a string". The breadcrumb has room for a branch name, not for
/// forty hex digits.
pub const SHORT_HASH: usize = 7;

/// What `HEAD` points at.
///
/// Two cases because the breadcrumb wants to render them differently: a branch
/// is a place you are, a detached hash is a state you are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// `ref: refs/heads/<name>` — the name, with `refs/heads/` stripped. Also
    /// what an unborn branch looks like (a fresh `git init` before the first
    /// commit), which is correct: the branch is real, it just has no commits.
    Branch(String),
    /// A raw object id, abbreviated to [`SHORT_HASH`].
    Detached(String),
}

impl Head {
    /// The text to put in the breadcrumb, either way.
    pub fn label(&self) -> &str {
        match self {
            Head::Branch(s) | Head::Detached(s) => s,
        }
    }

    pub fn is_detached(&self) -> bool {
        matches!(self, Head::Detached(_))
    }
}

/// The work-tree root containing `path`, if any.
///
/// Walks up looking for a `.git` entry of any kind — a directory (the ordinary
/// case), a file (a linked worktree, or a submodule since git 1.7.8), or even a
/// symlink to either. Existence is the whole test: deciding whether the thing is
/// a *valid* repository is [`git_dir`]'s job, and a caller that only wants "does
/// this row live in a repo" should not pay for it.
///
/// `path` may be a file or a directory; a file starts the walk at its parent.
/// Bare repositories are deliberately not found — they have no work tree, so
/// there are no rows to decorate.
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };

    let mut here = Some(start);
    for _ in 0..MAX_WALK_DEPTH {
        let dir = here?;
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Some(dir.to_path_buf());
        }
        here = dir.parent();
    }
    None
}

/// The directory holding `HEAD`, `refs` and the index for the repo rooted at
/// `root`.
///
/// Usually `root/.git`. When `.git` is a *file* it is a redirect — the linked
/// worktree and submodule layouts both use it — and its single `gitdir:` line
/// names the real directory, absolute or relative to `root`. Following it is not
/// optional: in a worktree created by `git worktree add`, `HEAD` only exists at
/// the far end of that redirect, so skipping it means every worktree shows no
/// branch.
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot = root.join(".git");
    let meta = std::fs::metadata(&dot).ok()?;
    if meta.is_dir() {
        return Some(dot);
    }
    if !meta.is_file() {
        return None;
    }
    let text = read_capped(&dot, MAX_GITFILE_BYTES)?;
    let target = parse_gitfile(&text)?;
    let target = Path::new(&target);
    if target.is_absolute() {
        Some(target.to_path_buf())
    } else {
        // Relative gitdirs are relative to the directory holding the `.git`
        // file, which is `root` — not to the process's cwd, which is why this
        // cannot just be `PathBuf::from`.
        Some(root.join(target))
    }
}

/// The `gitdir:` payload of a `.git` file.
///
/// Split out so the format is a table test rather than a filesystem test. Takes
/// bytes because a path is bytes; returns a `String` because a git-managed path
/// that is not UTF-8 is a fight not worth having here (and [`git_dir`] simply
/// finds nothing, which degrades to "no branch shown").
pub fn parse_gitfile(text: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(text).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("gitdir:") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// What `HEAD` points at for the repo rooted at `root`.
pub fn head(root: &Path) -> Option<Head> {
    let dir = git_dir(root)?;
    let text = read_capped(&dir.join("HEAD"), MAX_HEAD_BYTES)?;
    parse_head(&text)
}

/// The branch name (or short hash) for the breadcrumb.
pub fn branch(root: &Path) -> Option<String> {
    head(root).map(|h| h.label().to_string())
}

/// The contents of a `HEAD` file, as a [`Head`].
///
/// Pure, so detached, symbolic, unborn and corrupt all get a fixture. A symbolic
/// ref outside `refs/heads/` (a `HEAD` pointing at a tag ref, which git itself
/// will not write but which exists in the wild) keeps its full ref name rather
/// than being mangled into a branch that does not exist.
pub fn parse_head(text: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(text).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(reference) = text.strip_prefix("ref:") {
        let reference = reference.trim();
        if reference.is_empty() {
            return None;
        }
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return Some(Head::Branch(name.to_string()));
    }
    // Detached: a bare object id. Accept both sha-1 (40) and sha-256 (64), and
    // require it to be hex so that a `HEAD` full of junk reads as "no branch"
    // instead of putting the junk in the breadcrumb.
    let is_oid =
        text.len() >= SHORT_HASH && text.len() <= 64 && text.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_oid {
        return None;
    }
    let short: String = text.chars().take(SHORT_HASH).collect();
    Some(Head::Detached(short))
}

/// Read at most `cap` bytes, or nothing at all.
///
/// `None` covers every failure identically — missing, unreadable, a directory —
/// because every caller here does the same thing with all three: show no git
/// information. There is nothing for the user to act on in "your `.git/HEAD` is
/// not readable" that they will not also see the moment they run `git` in that
/// directory themselves.
fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(cap).read_to_end(&mut buf).ok()?;
    Some(buf)
}
