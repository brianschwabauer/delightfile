//! What git thinks of the files on screen.
//!
//! PLAN §7.3's first bullet: a status dot per row, gitignored files dimmed, the
//! branch in the breadcrumb, a dirty count beside it. All of that is one
//! question — "what is the state of every path under this repository" — asked
//! once per repository and answered from a cache, never per row and never on
//! the event loop.
//!
//! ## Two sources, deliberately
//!
//! - **`.git` read directly** ([`repo`]) for the things that are a file read:
//!   where the repository root is, and what `HEAD` points at. Walking up for a
//!   `.git` costs one `stat` per level and has to happen for *every* directory
//!   the user enters, so it cannot be a subprocess. Reading `HEAD` is 41 bytes
//!   and gives the branch name instantly, before any status has come back —
//!   which is what the breadcrumb needs, because a breadcrumb that pops in a
//!   second late reads as a bug.
//! - **`git status` shelled out** ([`status`]) for the thing that is not: which
//!   paths are modified, staged, untracked or ignored. Reimplementing that
//!   means the index format, every `.gitignore` precedence rule, submodules and
//!   `core.fileMode` — a year of work to be subtly wrong at, when the answer is
//!   sitting behind one process that is already installed. So the porcelain is
//!   *parsed* here, exactly, rather than approximated.
//!
//! The parse is the part worth owning, and it is the part that is unit-tested:
//! [`status::parse_porcelain_v2`] is a pure function from bytes to a status map,
//! so every entry type, every rename, and every filename with a newline in it is
//! a fixture rather than a live repository.
//!
//! ## Never on the caller's thread
//!
//! [`cache::Git`] is the whole public surface for status. Asking it for a path's
//! status is a hash lookup that returns whatever is currently known — possibly
//! nothing, possibly a second-old answer — and queues a refresh if one is due.
//! The refresh happens on one worker thread and rings the app's
//! [`Notifier`](crate::fs::Notifier) when it lands, the same handshake
//! [`crate::fs::scan`] and [`crate::preview::job`] use. A repaint reads the
//! [`cache::Git::generation`] counter to know whether anything changed.
//!
//! Consequently there is no code path in here that can make a keystroke wait on
//! a subprocess, and none that can make it wait on a lock held by a subprocess:
//! the worker parses into a fresh [`status::StatusData`] and swaps it in under
//! the lock in one move.
//!
//! ## When git is not installed
//!
//! The feature is silently absent. [`cache::Git::available`] goes false the
//! first time a spawn fails with `NotFound`, every status lookup returns `None`
//! from then on, and nothing is logged twice. A file manager on a machine
//! without git is a file manager, not a file manager with an error in it. The
//! `.git`-reading half still works, because it never needed the binary — a repo
//! root and a branch name are still shown.

pub mod cache;
pub mod repo;
pub mod status;

#[cfg(test)]
mod tests;

pub use cache::{Git, RepoStatus, GIT_REPOS, ROOT_MEMO};
pub use repo::{
    branch, git_dir, head, parse_gitfile, parse_head, repo_root, Head, MAX_GITFILE_BYTES,
    MAX_HEAD_BYTES, MAX_WALK_DEPTH, SHORT_HASH,
};
pub use status::{
    parse_porcelain_v2, status_blocking, DirtyCounts, FileStatus, StatusData, StatusError,
    MAX_STATUS_BYTES, MAX_STATUS_ENTRIES,
};
