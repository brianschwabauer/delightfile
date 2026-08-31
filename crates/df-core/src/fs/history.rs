//! Per-tab back/forward, for `Alt+←` and `Alt+→` (PLAN §4.1).
//!
//! The plan retires yazi's `H`/`L` for browser semantics, and browser semantics
//! are a specific, well-known thing that people already have intuitions about,
//! so this is the browser's model exactly: two stacks around a current
//! location, and **going somewhere new after going back throws the forward
//! stack away**. That last rule is the one that makes the model comprehensible
//! — a forward stack that survived a detour would offer to take you to a place
//! that is no longer on any path you walked.
//!
//! One history per tab, because a tab *is* a browsing session; opening a new
//! tab does not inherit where the old one has been.

use std::path::{Path, PathBuf};

/// How many directories back you can go.
///
/// 256 is far past the point of usefulness — nobody navigates 256 directories
/// and then wants the first one — and exists only so that a long-lived session
/// or a runaway script cannot grow the stack without bound. At roughly a
/// hundred bytes a path this is a few tens of kilobytes, which is cheaper than
/// thinking about it.
pub const HISTORY_LIMIT: usize = 256;

/// Where a tab has been.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    current: PathBuf,
}

impl History {
    pub fn new(start: impl Into<PathBuf>) -> History {
        History {
            back: Vec::new(),
            forward: Vec::new(),
            current: start.into(),
        }
    }

    pub fn current(&self) -> &Path {
        &self.current
    }

    /// Go somewhere new. Pushes the old location onto the back stack and drops
    /// the forward stack.
    ///
    /// Navigating to where you already are is a no-op rather than a duplicate
    /// entry: re-entering the current directory (a rescan, a `g h` while home,
    /// `cd .`) must not cost a press of `Alt+←` to undo.
    pub fn push(&mut self, dir: impl Into<PathBuf>) {
        let dir = dir.into();
        if dir == self.current {
            return;
        }
        self.forward.clear();
        let previous = std::mem::replace(&mut self.current, dir);
        self.back.push(previous);
        if self.back.len() > HISTORY_LIMIT {
            // Drop the oldest. `remove(0)` is a memmove of at most 256 pointers
            // and happens once per navigation past the cap — a `VecDeque` would
            // buy nothing but a less obvious type.
            self.back.remove(0);
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// `Alt+←`. `None` at the beginning of history, so the caller can leave the
    /// button dim rather than pretending something happened.
    pub fn go_back(&mut self) -> Option<&Path> {
        let previous = self.back.pop()?;
        let current = std::mem::replace(&mut self.current, previous);
        self.forward.push(current);
        Some(&self.current)
    }

    /// `Alt+→`.
    pub fn go_forward(&mut self) -> Option<&Path> {
        let next = self.forward.pop()?;
        let current = std::mem::replace(&mut self.current, next);
        self.back.push(current);
        Some(&self.current)
    }

    /// Everything behind the cursor, oldest first — what the `z` jump overlay
    /// and the command palette list as "recent" (PLAN §7.2, §4.4).
    pub fn back_stack(&self) -> &[PathBuf] {
        &self.back
    }

    pub fn forward_stack(&self) -> &[PathBuf] {
        &self.forward
    }
}
