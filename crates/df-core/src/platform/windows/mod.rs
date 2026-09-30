//! Windows: bodies where one `std` call or one CRT function does the job, and
//! stubs everywhere else (the native bodies are Phase 4,
//! `plans/other-platforms/04-windows.md`).

pub mod defaults;
pub mod dirs;
pub mod errno;
pub mod fs;
pub mod keys;
pub mod meta;
pub mod os;
pub mod pipe;
pub mod process;
pub mod socket;
pub mod time;
pub mod trash;
pub mod user;

pub use super::stub::{nofollow, thread, watch, xattr};
