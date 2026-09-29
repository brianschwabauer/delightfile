//! The Linux bodies: the program as it ran before there was a seam, moved
//! here rather than rewritten, so that `git log --follow` on any of these
//! files is the history of the code in it.
//!
//! `dbus` and `wayland` are private to this module: they are the machinery
//! the bodies below are built on, and nothing outside `platform/` may name
//! them, because no other target has them.

pub mod appearance;
mod dbus;
pub mod device;
pub mod mounts;
/// `--portal`: the xdg-desktop-portal file-chooser backend, and
/// `org.freedesktop.FileManager1` ("Show in folder").
pub mod portal;
mod wayland;
pub mod window;
