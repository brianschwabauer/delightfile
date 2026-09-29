//! Running a snippet on macOS: the shell body Linux and macOS share
//! ([`crate::platform::unix::open`]), not yet detached from the window.
//!
//! macOS has no `setsid` program; its own way out of delightfile's process
//! group, `process_group(0)` on the spawn, is Phase 2's
//! (`plans/other-platforms/02-macos.md` M2.16). Until then a launched
//! program runs parented to the window — what Linux does when `setsid` is
//! missing: worse than detached, much better than not opening the file.

pub use crate::platform::unix::open::*;

/// The argv as it is: nothing detaches it here yet (M2.16).
pub fn detached_argv(argv: Vec<String>) -> Vec<String> {
    argv
}
