//! The platform seam: everything in df-core whose body depends on the operating
//! system, behind one module that offers the same set of functions on every
//! target.
//!
//! delightfile was written for Linux, and most of it is plain Rust that does
//! not care. The parts that do care — a syscall `std` does not expose, a
//! Unix-only extension trait, a freedesktop convention — live under this module
//! and nowhere else. Code outside it is written once and calls
//! `platform::<module>::<function>`; it never sees a `cfg`.
//!
//! Inside, each target has a module of its own (`linux`, `macos`, `windows`)
//! that provides exactly the set in the table below; `unix` holds the bodies
//! Linux and macOS share. Selection is `#[cfg]` on the `mod` lines and a glob
//! `pub use`, with no traits and no `dyn`: three implementations and one caller
//! do not need them, and the CI matrix compiling all three targets is what keeps
//! the sets in step (`plans/other-platforms/00-ground-rules.md` §2).
//!
//! A target that has no implementation of a feature yet gets an honest stub:
//! an operation fails with [`crate::DfError::Unsupported`], a query answers
//! "nothing", and a service comes up inert. A stub never panics and never
//! claims to have done work it did not do.
//!
//! # The contract
//!
//! Every function and constant the rest of the crate (and df-app) may use, and
//! what each target does for it. A signature changes here in the same change
//! that changes it in the target modules.
//!
//! | Module | Item | Linux | macOS | Windows |
//! |---|---|---|---|---|

#[cfg(unix)]
mod unix;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
