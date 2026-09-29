//! The Windows bodies: for now, stand-ins with the Linux names and
//! signatures that answer "not available here", so the program builds and
//! runs while Phase 4 (`plans/other-platforms/04-windows.md`) writes the
//! native ones.

/// Whether `--portal` exists: there is no desktop portal here, so the flag
/// is an unknown option and `--help` does not mention it.
pub const HAS_PORTAL: bool = false;

pub mod appearance;
pub mod cli;
pub mod clipboard;
pub mod device;
pub mod fonts;
pub mod gfx;
pub mod mounts;
pub mod pdfium;
pub mod portal;
pub mod window;
