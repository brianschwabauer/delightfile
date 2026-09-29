//! macOS: the shared Unix bodies, macOS's own where Linux's do not carry over
//! (`plans/other-platforms/02-macos.md`), and stubs where neither exists yet.

pub use super::stub::{nofollow, thread, trash, xattr};
pub use super::unix::{dirs, errno, meta, os, pipe, socket, time};

mod kqueue;
pub mod process;
pub mod watch;

/// The shared Unix file primitives, with the Linux-only ones (reflink,
/// dropping cached pages, `statfs` magic) answered by the stubs.
pub mod fs {
    pub use crate::platform::stub::fs::*;
    pub use crate::platform::unix::fs::*;
}

/// The shared Unix uid, with no owner names until M2.4.
pub mod user {
    pub use crate::platform::stub::user::*;
    pub use crate::platform::unix::user::*;
}
