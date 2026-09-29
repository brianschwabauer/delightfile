//! No parent-death signal: stands in on Windows until W4.31 (a job object
//! that kills its members when the last handle closes). macOS leaves a
//! watcher on the parent's exit (`platform/macos/process.rs`).

use std::process::Command;

/// Nothing is arranged: on this platform a child started for a worker thread
/// **can outlive a parent that crashes** — an abort, a kill, anything that
/// skips destructors. An ordinary exit still stops it, since the owner drops
/// it with [`crate::platform::process::terminate`] on the way out.
pub fn tie_to_this_thread(_command: &mut Command) {}
