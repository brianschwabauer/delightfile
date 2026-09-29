//! Linux: the platform delightfile was written on, and the one every body here
//! was moved from unchanged.

pub mod fs;
mod inotify;
pub mod thread;
pub mod trash;
pub mod watch;

pub use super::unix::{dirs, errno, meta, pipe, process, time, user};
