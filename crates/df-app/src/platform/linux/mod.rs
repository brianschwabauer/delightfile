//! The Linux bodies: the program as it ran before there was a seam, moved
//! here rather than rewritten, so that `git log --follow` on any of these
//! files is the history of the code in it.
//!
//! `dbus`, `gio` and `wayland` are private to this module: they are the
//! machinery the bodies below are built on, and nothing outside `platform/`
//! may name them, because no other target has them.

/// Whether `--portal` exists: there is a desktop portal to serve.
pub const HAS_PORTAL: bool = true;

/// Whether a file on this machine has an owner and POSIX permission bits to
/// show and to change: every one does.
pub const POSIX_PERMISSIONS: bool = true;

pub mod appearance;
pub mod cli;
pub mod clipboard;
mod dbus;
pub mod device;
pub mod fonts;
pub mod gfx;
mod gio;
pub mod keys;
pub mod mounts;
pub mod open;
pub mod pdfium;
pub use crate::platform::unix::process;
/// `--portal`: the xdg-desktop-portal file-chooser backend, and
/// `org.freedesktop.FileManager1` ("Show in folder").
pub mod portal;
pub mod svg;
pub mod trash;
mod wayland;
pub mod window;
