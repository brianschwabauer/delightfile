//! macOS's process parts: the ones shared with Linux (the null device, pause
//! and resume, `terminate`), what a Mac is told when its `rsync` is too old,
//! and, until M2.29, no parent-death signal.

pub use crate::platform::stub::process::*;
pub use crate::platform::unix::process::*;

/// What to add to "needs rsync" when [`crate::sync::rsync::available`] says
/// no. A Mac always has an `rsync`, and it is always too old
/// ([`crate::sync::rsync::MIN_VERSION`]): Apple ships Samba's 2.6.9 or
/// openrsync, so the one to install is Homebrew's.
pub const RSYNC_HINT: &str = "needs rsync 3.1 or newer — `brew install rsync`";
