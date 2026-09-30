//! The macOS bodies: for now, stand-ins with the Linux names and signatures
//! that answer "not available here", so the program builds and runs while
//! Phase 2 (`plans/other-platforms/02-macos.md`) writes the native ones.

/// Whether `--portal` exists: there is no desktop portal here, so the flag
/// is an unknown option and `--help` does not mention it.
pub const HAS_PORTAL: bool = false;

/// Whether a file on this machine has an owner and POSIX permission bits to
/// show and to change: every one does.
pub const POSIX_PERMISSIONS: bool = true;

pub mod appearance;
pub mod cli;
pub mod clipboard;
pub mod device;
pub mod fonts;
pub mod gfx;
pub mod keys;
pub mod menubar;
pub mod mounts;
pub mod open;
pub mod pdfium;
pub mod portal;
pub use crate::platform::unix::process;
pub mod trash;
pub mod window;
