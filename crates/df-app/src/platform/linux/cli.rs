//! The lines of `--help` that differ by platform ([`crate::cli::usage`]):
//! on Linux, the two flags the portal starts windows with say so, and
//! `--portal` itself is described.

/// The `--reveal` entry.
pub const REVEAL_USAGE: &str =
    "  --reveal               show the path rather than open it: its folder opens
                         with the cursor on it, a folder included (what
                         \"Show in folder\" asks --portal for; needs a path,
                         and cannot be a --chooser-* dialog)
";

/// The `--chooser-request` entry.
pub const REQUEST_USAGE: &str = "  --chooser-request=<path>
                         read the whole dialog from this file instead: its
                         kind, title, button label, suggested name, starting
                         folder and file-type filters (written by --portal;
                         needs --chooser-file, and outranks the switches)
";

/// The flags only this platform has, after the chooser's: `--portal`.
pub const EXTRA_USAGE: &str =
    "  --portal               serve the xdg-desktop-portal file chooser, and
                         org.freedesktop.FileManager1 (\"Show in folder\"), on
                         the session bus; D-Bus starts this, not a person
";
