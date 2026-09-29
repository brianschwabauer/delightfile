//! macOS: the shared Unix bodies, macOS's own where Linux's do not carry over
//! (`plans/other-platforms/02-macos.md`), and stubs where neither exists yet.

pub use super::stub::{nofollow, thread, trash, xattr};
pub use super::unix::{dirs, errno, meta, os, pipe, socket, time};

pub mod fs;
mod kqueue;
pub mod process;
pub mod user;
pub mod watch;
