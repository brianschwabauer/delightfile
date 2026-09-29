//! macOS: the shared Unix bodies, and stubs where Linux has something macOS
//! does not (the native bodies are Phase 2, `plans/other-platforms/02-macos.md`).

pub use super::stub::{thread, trash, watch, xattr};
pub use super::unix::{dirs, errno, meta, os, pipe, process, time};

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
