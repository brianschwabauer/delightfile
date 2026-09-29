//! macOS: the shared Unix bodies, macOS's own where Linux's do not carry over
//! (`plans/other-platforms/02-macos.md`), and stubs where neither exists yet.

pub use super::stub::xattr;
pub use super::unix::{dirs, errno, meta, os, pipe, socket, time};

pub mod fs;
mod kqueue;
pub mod nofollow;
pub mod process;
pub mod thread;
pub mod trash;
pub mod user;
pub mod watch;
