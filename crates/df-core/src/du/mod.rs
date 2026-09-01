//! Recursive directory sizes: the size column's missing number, and the ncdu
//! replacement (PLAN §7.3, PLAN §4.1).
//!
//! Two features share one walk, which is why they share one module.
//!
//! The first is small and constant: [`crate::fs::Entry::len`] is zero for
//! directories, on purpose, because nothing cheap knows how big a directory is.
//! That makes the size sort put every directory in one indistinguishable block
//! and the size linemode draw a dash. Filling it in is a recursive walk, so it
//! cannot happen during a scan — but it can happen *behind* one, arriving a
//! second later and rewriting the column in place.
//!
//! The second is **"what's big" mode**: a drill-down where every row carries a
//! bar showing its share of the parent, and the numbers count up while the walk
//! runs. That is the same data — per-directory recursive totals — asked for at
//! depth 1 and sorted descending.
//!
//! So the module is one walker ([`mod@walk`]), one pool that runs it off the event
//! loop ([`scanner`]), and one bounded cache that makes the answers survive
//! leaving the directory ([`cache`]). The interesting decisions each live next
//! to the code they constrain:
//!
//! - **Blocks and apparent size, both, always** — [`mod@walk`]'s essay on why a
//!   sparse VM image and a directory of tiny files break any single number.
//! - **What is skipped**: symlinks always, other filesystems by default,
//!   hardlinked content after the first sighting — also [`mod@walk`].
//! - **The cache is advisory** — [`cache`]'s essay on why a directory's `mtime`
//!   cannot detect a change three levels down, and why the honest response is to
//!   say so rather than to fake a freshness check.
//! - **Supersede, cancellation and generation tokens** — [`scanner`], which is
//!   `fs::scan`'s twin and says where it differs.
//!
//! ## The shape of using it
//!
//! ```no_run
//! use df_core::du::{DuMessage, DuScanner};
//! use df_core::fs::no_notifier;
//!
//! let du = DuScanner::start(no_notifier());
//! let token = du.request("/home/brian/Downloads", 1);
//! // …later, when the notifier has rung:
//! for message in du.drain() {
//!     if message.token() != token {
//!         continue; // a walk nobody is looking at any more
//!     }
//!     if let DuMessage::Progress { updates, .. } = message {
//!         for update in updates {
//!             // update.dir now weighs update.total_bytes, and will weigh more
//!             // if update.done is false.
//!         }
//!     }
//! }
//! let biggest = du.heavy_hitters(std::path::Path::new("/home/brian/Downloads"));
//! ```

pub mod cache;
pub mod scanner;
pub mod walk;

#[cfg(test)]
mod tests;

pub use cache::{
    current_mtime, DuCache, DuRecord, HeavyHitter, DU_CACHE_DIRS, MAX_CACHED_CHILDREN,
    MAX_CACHE_CHILDREN,
};
pub use scanner::{du_blocking, DuMessage, DuScanner, DuToken, DU_WORKERS, MAX_TRACKED_DIRS};
pub use walk::{
    child_counts, crosses_boundary, walk, walk_blocking, DuOptions, DuTotals, DuUpdate, BLOCK_UNIT,
    CANCEL_CHECK_ENTRIES, DU_BATCH, MAX_COUNTED_CHILDREN, MAX_DEPTH, MAX_HARDLINK_ENTRIES,
    UPDATE_INTERVAL,
};
