//! The Windows bodies (`plans/other-platforms/04-windows.md`): the console,
//! the clipboard, the drives card, the openers, the pointer, the window's
//! size and its title bar, the appearance. What has no body yet — a drag out
//! (W4.27), a phone — answers "not available here" with the Linux names and
//! signatures.

/// Whether `--portal` exists: there is no desktop portal here, so the flag
/// is an unknown option and `--help` does not mention it.
pub const HAS_PORTAL: bool = false;

/// Whether a file on this machine has an owner and POSIX permission bits to
/// show and to change: none does. A Windows file has ACLs, which nine bits
/// cannot show, and one read-only attribute; so the spot panel has no owner
/// or permissions row for one, the linemodes say what little there is, and
/// the permissions card is not offered (`plans/other-platforms/04-windows.md`
/// W4.29, decided with Brian 2026-09-29). A server's file keeps both.
pub const POSIX_PERMISSIONS: bool = false;

pub mod appearance;
pub mod cli;
pub mod clipboard;
pub mod device;
pub mod fonts;
pub mod gfx;
pub mod keys;
pub mod mounts;
pub mod open;
pub mod pdfium;
pub mod portal;
pub mod print;
pub mod process;
mod titlebar;
pub mod trash;
pub mod window;
