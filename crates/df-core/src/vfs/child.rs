//! The rclone daemon's life, tied to ours: a parent-death signal set between
//! `fork` and `exec` on Linux, a job object it is put in once started on
//! Windows, and a gentle `SIGTERM` to stop it. All are the platform's
//! ([`crate::platform::process`]); this module is the name the vfs and
//! df-app's gvfs watcher already call them by.

pub use crate::platform::process::{tie, tie_to_this_thread, Tie};

pub(super) use crate::platform::process::terminate;
