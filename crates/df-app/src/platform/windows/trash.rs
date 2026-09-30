//! The trash on Windows is the Recycle Bin, and delightfile does not look
//! inside it (`plans/other-platforms/04-windows.md` W4.7, W4.8).
//!
//! `d` sends a file there through `SHFileOperationW`, which says the file
//! went but not where the bin keeps it, and the bin's own names and its
//! restore are COM (`IShellFolder`, `IContextMenu`), deferred with their
//! design recorded (W4.28). So there is no trash view here: the trash's
//! doors open the Recycle Bin in Explorer, where a file is restored or the
//! bin emptied ([`SYSTEM_BIN`]), and `u` after a trash says that is where to
//! go ([`RESTORED_ELSEWHERE`]) rather than failing to find the file.

/// The line under an empty trash view: there is no trash view here.
pub const LISTED_NOTE: Option<&str> = None;

/// What "Empty trash" adds to its toast: nothing.
pub const EMPTIED_NOTE: Option<&str> = None;

/// The Recycle Bin as the shell names it, opened in Explorer in place of the
/// trash view.
pub const SYSTEM_BIN: Option<&str> = Some("shell:RecycleBinFolder");

/// What `u` after a trash says: the bin keeps no name this program can
/// find the file by again.
pub const RESTORED_ELSEWHERE: Option<&str> = Some("Restore it from the Recycle Bin");
