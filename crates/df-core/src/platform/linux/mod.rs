//! Linux: the platform delightfile was written on, and the one every body here
//! was moved from unchanged.

pub mod fs;
mod inotify;
pub mod nofollow;
pub mod process;
pub mod thread;
pub mod trash;
pub mod user;
pub mod watch;
pub mod xattr;

pub use super::unix::{dirs, errno, meta, os, pipe, time};
