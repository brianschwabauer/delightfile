//! The bodies Linux and macOS share: everything that is POSIX, or `std`'s
//! Unix extension traits, and means the same on both.
//!
//! Nothing here is reached directly. `linux` and `macos` each re-export what
//! they take from it, so the contract in [`super`] is always read off a target
//! module, and a macOS body that has to diverge later replaces one re-export
//! rather than a caller.

pub mod errno;
pub mod fs;
