//! macOS: the shared Unix bodies, and stubs where Linux has something macOS
//! does not (the native bodies are Phase 2, `plans/other-platforms/02-macos.md`).

pub use super::stub::{thread, trash, watch};
pub use super::unix::{dirs, errno, meta, pipe, time, user};

/// The shared Unix file primitives, with the Linux-only ones (reflink,
/// dropping cached pages, `statfs` magic) answered by the stubs.
pub mod fs {
    pub use crate::platform::stub::fs::*;
    pub use crate::platform::unix::fs::*;
}
