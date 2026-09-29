//! macOS: the shared Unix bodies, and macOS's own where Linux's do not carry
//! over (`plans/other-platforms/02-macos.md`).

pub use super::unix::{errno, meta, os, pipe, socket, time};

mod bplist;
pub mod dirs;
pub mod fs;
mod kqueue;
pub mod nofollow;
pub mod process;
pub mod thread;
pub mod trash;
pub mod user;
pub mod watch;
pub mod xattr;
