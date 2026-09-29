//! The rclone daemon's life, tied to ours: a parent-death signal set between
//! `fork` and `exec`, and a gentle `SIGTERM` to stop it. Both are the
//! platform's ([`crate::platform::process`]); this module is the name the vfs
//! and df-app's gvfs watcher already call them by.

pub use crate::platform::process::tie_to_this_thread;

pub(super) use crate::platform::process::terminate;
